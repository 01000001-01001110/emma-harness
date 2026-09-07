//! The Code page: an in-TUI file browser, viewer, and per-file history.
//!
//! It is a **page** in the exact sense Settings, Memory and Harness are: it
//! owns `r.main` between the sidebar and the bottom bar, one occupant at a
//! time, opened by `Alt+c` and by the sidebar's TOOLS **Code** row (owner
//! ruling, 2026-09-06). The external editor that `Alt+c` used to launch has
//! not gone anywhere — [`CodeAction::LaunchEditor`], on `F7` and on the
//! header's `[Editor]` button, is the same
//! [`usertools::launch`](crate::usertools::launch) call the chord used to
//! make, so `plan_code`'s settings chain and PATH probe stay live and
//! reachable. One row, two doors.
//!
//! Three regions ([`split`]): an EXPLORER file tree on the left, and on the
//! right a header over either the open file's contents (FILE) or its commit
//! history over the patch the selected commit introduced (HISTORY).
//!
//! **Two pure halves, the Memory rule.** This module draws a [`CodeView`] and
//! maps one key to one [`CodeAction`]; it never reads a file and never shells
//! to git. The shell does the IO through [`super::code_git`] and hands the
//! answer back through [`CodeView::set_open`], [`CodeView::set_history`] and
//! [`CodeView::set_diff`], so the page always shows disk truth and the git
//! calls can be moved off the input thread without touching this file. **They
//! must be**: `git log --follow` and `git show` were measured at 56–475 ms on
//! this 289-commit repository (2026-09-06, Windows), and anything over ~50 ms
//! under the frame lock is a terminal that stops answering. The three setters
//! are the seam that makes a worker thread possible; the page draws
//! "loading history…" until one answers.
//!
//! **Nothing raw from disk reaches a cell.** File bytes are sanitised at read
//! time, in [`OpenFile::from_read`], through the one sanitiser this codebase
//! has ([`super::markdown::sanitise`]) — a TAB is one ratatui cell and an
//! unknown number of terminal columns, and an ESC in a cell is a control
//! sequence. Commit subjects and diff rows are sanitised at paint time
//! instead, because nothing indexes into them; the file's lines are done at
//! read time because the editor's cursor arithmetic (stage b) is in chars of
//! the buffer and would desynchronise from the screen if the two disagreed.
//!
//! All width arithmetic is in display columns via [`cols`]/[`fit`]; no row
//! writes past its pane's inner width. **The document body included, since
//! stage b.** Stage a drew the file through [`fit`] and had no cursor, so a
//! wide glyph could not put one in the wrong place. It can now, and the answer
//! is that the buffer counts in *chars* ([`OpenFile::col`]) and the paint
//! converts to columns at the one place that draws a cell — [`doc_geom`] and
//! [`cell_to_pos`] read the same conversion in opposite directions, so a press
//! and a paint cannot disagree about which character a column is.
//!
//! **A buffer is written back or it is not writable, and the header says
//! which.** Sanitising is what makes bytes drawable and it is also what makes
//! them no longer the file: a buffer showing four spaces where the file has a
//! TAB is not that file, so [`OpenFile::locked`] refuses the save rather than
//! detabbing somebody's source. The second lock is subtler and catches the
//! same class from the other end — the shell hands [`OpenFile::from_read`] the
//! hash of the bytes on disk, the buffer computes the hash of the bytes it
//! *would write back unedited*, and a file whose two answers disagree (mixed
//! line terminators, or a file rewritten between the shell's two reads) is
//! read-only too. What survives both locks can be saved byte-for-byte, and
//! [`super::code_git::save_file`] still refuses on a stale hash underneath.
//!
//! **The chat strip asks about the file, and says what it sent.** Three rows
//! under the document: a rule, a pointer to the last question,
//! and one input line. Submitting composes an attributed line
//! ([`compose_ask`]) naming the file and — when the whole of it will not fit
//! under [`MAX_ASK_BODY`] — the exact line range that did, and hands it out as
//! [`CodeAction::Ask`]. The shell puts that on the same channel a typed line
//! takes, so nothing here decides whether a goal is running: the steering
//! queue answers that, and it is the same answer either way. The strip does
//! **not** render the transcript. The answer streams into the main transcript
//! like every other one, and a second renderer for one conversation is two
//! views that can disagree.
//!
//! **The language server decorates; it never delays.** Diagnostics land in the
//! gutter beside the lines they are about and under the exact span the server
//! named, `F5` asks what is under the cursor and `F6` where it is defined,
//! `F8` asks where it is used and `F10` what the file contains, and
//! one row under the document carries the server's state, the count and the
//! message the cursor is sitting on. **Nothing in this module talks to a
//! server**: it holds an [`Lsp`] that only an [`LspUpdate`] writes, delivered
//! by the bridge in [`super::code_lsp`], which owns every `.await` in the
//! feature. That is what makes a dead or indexing server cost the page its
//! decorations and never a keystroke - and it is why every variant of
//! [`LspStatus`] is a sentence rather than an absence: a page with no
//! decorations because the server is still indexing and a page with no
//! decorations because the file has no problems must not look the same.

use std::collections::BTreeSet;
use std::path::PathBuf;

use ratatui::buffer::Buffer;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Widget};

use super::code_git::{hash_bytes, joined, Commit, DiffKind, DiffRow, FileRead, LineEnding, Saved};
use super::markdown::sanitise;
use super::palette::Role;
use super::render::{cols, fit, Skin};

// region: State
// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

/// Which view the right pane shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    #[default]
    File,
    History,
}

/// Which region has the keyboard. The valid set depends on [`Mode`]: File has
/// Tree, Body and Chat; History has Tree, Commits, Diff and Chat.
///
/// `Chat` is the strip's, and it is last in both cycles because it is the one
/// region that is reachable in either mode — the strip asks about the open
/// file whether the right pane is showing the file or its history.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Focus {
    #[default]
    Tree,
    Body,
    Commits,
    Diff,
    /// The chat strip's input line.
    Chat,
}

/// One entry in the flattened file tree — a directory or a file, at a depth.
/// The full set is built once ([`build_nodes`]); which of them are *visible*
/// is derived from [`CodeView::expanded`] at read time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node {
    /// Repository-relative, `/`-separated.
    pub path: String,
    /// The last path segment — what the row shows.
    pub name: String,
    pub depth: usize,
    pub is_dir: bool,
}

/// The suffix the header adds to a file whose bytes had to be changed to be
/// drawn safely. Short because it shares the header row with the file's own
/// name, and a marker that gets ellipsised is a marker nobody reads.
///
/// It is not decoration and it is not tidiness: it is the note stage b's save
/// path reads to refuse the write. A buffer that says `\t` on disk and four
/// spaces on screen is not the file, and writing it back would detab somebody
/// else's source without being asked.
pub const SANITISED_NOTE: &str = "sanitised — not writable";

/// The header's note for a file this page cannot put back on disk unchanged.
///
/// Two different causes, one honest answer. A file with mixed terminators
/// (`\r\n` on most lines and `\n` on one) has no single `LineEnding` that
/// rewrites it as it was, so saving it would rewrite lines nobody edited. And
/// a file rewritten between the shell's read and its hash lands here too,
/// which is the safe direction: the page declines rather than writing an old
/// buffer over somebody's new bytes.
pub const NOT_EXACT_NOTE: &str = "cannot be rewritten exactly — not writable";

/// The open file: its lines and the editor's cursor over them, or the honest
/// refusal in `note` (binary, too large, not UTF-8) with no lines drawn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenFile {
    pub path: String,
    /// Sanitised at read time — see the module header.
    pub lines: Vec<String>,
    pub note: Option<String>,
    pub scroll: usize,
    /// Cursor line, an index into [`Self::lines`].
    pub line: usize,
    /// Cursor column, in **chars** of that line, `0..=len`.
    ///
    /// Chars and not display columns, because this number indexes the buffer a
    /// save writes; the paint is the half that converts to columns, and it is
    /// the only half that may.
    pub col: usize,
    /// The fixed end of a selection; the cursor is the moving end. `None` is
    /// no selection. Anchored to a `(line, char)` position in the document and
    /// never to a screen cell, so scrolling under a live drag cannot move it.
    pub anchor: Option<(usize, usize)>,
    /// Edited since the last read or write.
    pub dirty: bool,
    /// The terminator the file had on disk, passed back unchanged on save.
    /// This is the CRLF guarantee: a working tree checked out with `\r\n` is
    /// not silently normalised by having been opened here.
    pub ending: LineEnding,
    /// Whether the file ended with a terminator. Also passed back unchanged —
    /// inventing a final newline is the same defect in miniature.
    pub trailing_newline: bool,
    /// The bytes on disk this buffer agrees it came from, as a hash, or `None`
    /// when the two reads disagreed. It is what [`super::code_git::save_file`]
    /// compares against before it writes anything.
    pub hash: Option<u64>,
    /// Why the buffer cannot be written back, or `None` when it can. The
    /// header says it and [`CodeView::request_save`] refuses on it.
    pub locked: Option<&'static str>,
}

impl OpenFile {
    /// A file the shell has just read, made safe to draw.
    ///
    /// The sanitising happens **here** rather than in `code_git::read_file`
    /// for one reason: `read_file` is also what the save path compares bytes
    /// against, and a reader that quietly rewrites what it returns would make
    /// the round trip lie. The page is the consumer that needs cells, so the
    /// page is where the bytes become cells.
    ///
    /// `disk_hash` is [`super::code_git::file_hash`] over the same path,
    /// taken by the shell beside the read. It is compared against the hash of
    /// the bytes this buffer would write back with nothing edited; the two
    /// agree exactly when the file round-trips, and a buffer whose two answers
    /// disagree is locked rather than saved. Both locks are decided once,
    /// here, so nothing downstream has to re-derive them.
    pub fn from_read(path: String, read: FileRead, disk_hash: Option<u64>) -> Self {
        match read {
            FileRead::Text(text) => {
                let lines: Vec<String> = text.lines.iter().map(|l| sanitise(l)).collect();
                let changed = lines.iter().zip(&text.lines).any(|(a, b)| a != b);
                // The bytes an unedited save would put on disk. Computed from
                // the *unsanitised* lines, because that is what the file is.
                let round = joined(&text.lines, text.ending, text.trailing_newline);
                let ours = hash_bytes(round.as_bytes());
                let exact = disk_hash == Some(ours);
                let locked = if changed {
                    Some(SANITISED_NOTE)
                } else if !exact {
                    Some(NOT_EXACT_NOTE)
                } else {
                    None
                };
                Self {
                    path,
                    lines,
                    note: None,
                    scroll: 0,
                    line: 0,
                    col: 0,
                    anchor: None,
                    dirty: false,
                    ending: text.ending,
                    trailing_newline: text.trailing_newline,
                    hash: exact.then_some(ours),
                    locked,
                }
            }
            FileRead::Refused(why) => Self {
                path,
                lines: Vec::new(),
                note: Some(why),
                scroll: 0,
                line: 0,
                col: 0,
                anchor: None,
                dirty: false,
                ending: LineEnding::Lf,
                trailing_newline: true,
                hash: None,
                locked: None,
            },
        }
    }

    /// Whether there is text to draw. A refusal note means there is not, and
    /// the pane says the note instead of an empty region.
    pub fn has_text(&self) -> bool {
        self.note.is_none()
    }

    /// Whether keys may reach this document at all.
    ///
    /// Three conditions and every one of them is a refusal somebody would
    /// otherwise discover by losing work: there is no text (a refusal note), the
    /// bytes cannot be reproduced ([`Self::locked`]), or there is no hash to
    /// compare a save against. Typing into any of those would be a promise the
    /// page cannot keep.
    pub fn editable(&self) -> bool {
        self.note.is_none() && self.locked.is_none() && self.hash.is_some()
    }

    /// The selection as an ordered `(start, end)` pair of `(line, char)`, or
    /// `None` when nothing is selected or the two ends coincide.
    pub fn selection(&self) -> Option<((usize, usize), (usize, usize))> {
        let a = self.anchor?;
        let b = (self.line, self.col);
        if a == b {
            return None;
        }
        Some(if a <= b { (a, b) } else { (b, a) })
    }

    /// The selected text, exactly as it would be pasted back. Always `\n`
    /// between lines: this is going to a clipboard, not to disk, and
    /// [`Self::ending`] is the only thing that decides what disk gets.
    pub fn selected_text(&self) -> String {
        let Some((start, end)) = self.selection() else {
            return String::new();
        };
        if start.0 == end.0 {
            return self
                .chars(start.0)
                .get(start.1..end.1)
                .map(|c| c.iter().collect())
                .unwrap_or_default();
        }
        let mut out: String = self.chars(start.0).into_iter().skip(start.1).collect();
        for line in start.0 + 1..end.0 {
            out.push('\n');
            out.push_str(self.lines.get(line).map(String::as_str).unwrap_or(""));
        }
        out.push('\n');
        let last: String = self.chars(end.0).into_iter().take(end.1).collect();
        out.push_str(&last);
        out
    }

    /// The whole buffer, as it would go to a clipboard.
    pub fn whole_text(&self) -> String {
        joined(&self.lines, LineEnding::Lf, self.trailing_newline)
    }

    /// Remove the selection, leaving the cursor where it started. `true` when
    /// there was one, which is what makes typing and Backspace replace it.
    pub fn delete_selection(&mut self) -> bool {
        let Some((start, end)) = self.selection() else {
            self.anchor = None;
            return false;
        };
        let head: String = self.chars(start.0).into_iter().take(start.1).collect();
        let tail: String = self.chars(end.0).into_iter().skip(end.1).collect();
        let last = end.0.min(self.lines.len().saturating_sub(1));
        self.lines.drain(start.0..=last);
        self.lines.insert(start.0, format!("{head}{tail}"));
        self.line = start.0;
        self.col = start.1;
        self.anchor = None;
        self.dirty = true;
        true
    }

    /// Insert clipboard text at the cursor, replacing a live selection. A
    /// multi-line paste splits the current line the way typing Enter would.
    ///
    /// **Every pasted line goes through the same sanitiser the file's own
    /// bytes did**, and for the same reason: a clipboard is somebody else's
    /// bytes, and `\x1b` in a cell is a control sequence rather than a
    /// character. Doing it here rather than at paint time is what keeps the
    /// cursor's char arithmetic agreeing with the screen, and it is what keeps
    /// the buffer equal to what a save will write — so a paste does **not**
    /// lock the file, unlike a read that had to be changed. Answers whether
    /// sanitising altered anything, because a paste that quietly lost bytes is
    /// the `PasteLoss` defect [`super::input`] already pays for.
    pub fn paste(&mut self, text: &str) -> bool {
        self.delete_selection();
        self.clamp();
        // `\r\n` and a bare `\r` both arrive from real clipboards; neither is
        // a character this buffer keeps, and both are line breaks here.
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        let parts: Vec<String> = text.split('\n').map(sanitise).collect();
        let changed = parts.join("\n") != text;
        let Some((first, rest)) = parts.split_first() else {
            return changed;
        };
        let cs = self.chars(self.line);
        let head: String = cs[..self.col].iter().collect();
        let tail: String = cs[self.col..].iter().collect();
        if rest.is_empty() {
            let cur = format!("{head}{first}");
            self.col = cur.chars().count();
            self.lines[self.line] = format!("{cur}{tail}");
        } else {
            self.lines[self.line] = format!("{head}{first}");
            for (i, part) in rest.iter().enumerate() {
                let last = i + 1 == rest.len();
                let body = if last {
                    format!("{part}{tail}")
                } else {
                    part.clone()
                };
                self.lines.insert(self.line + 1 + i, body);
                if last {
                    self.line += i + 1;
                    self.col = part.chars().count();
                }
            }
        }
        self.dirty = true;
        changed
    }

    /// Start or extend a selection while the cursor moves, or drop it.
    fn mark(&mut self, extend: bool) {
        if extend {
            if self.anchor.is_none() {
                self.anchor = Some((self.line, self.col));
            }
        } else {
            self.anchor = None;
        }
    }

    fn chars(&self, line: usize) -> Vec<char> {
        self.lines
            .get(line)
            .map(|l| l.chars().collect())
            .unwrap_or_default()
    }

    fn line_len(&self, line: usize) -> usize {
        self.lines.get(line).map(|l| l.chars().count()).unwrap_or(0)
    }

    /// Clamp the cursor into the document after any edit or move. An empty
    /// buffer gains one empty line, so there is always a line to be on.
    fn clamp(&mut self) {
        if self.lines.is_empty() {
            self.lines.push(String::new());
        }
        self.line = self.line.min(self.lines.len() - 1);
        self.col = self.col.min(self.line_len(self.line));
    }

    /// Insert one typed character.
    ///
    /// A control character is dropped rather than inserted. Nothing on a
    /// keyboard produces one as a `KeyCode::Char`, but this is the one door
    /// into the buffer that a future caller could reach with anything, and a
    /// buffer that can hold `\x1b` is a redirected stream that can carry it.
    fn insert_char(&mut self, c: char) {
        if c.is_control() {
            return;
        }
        self.clamp();
        let mut cs = self.chars(self.line);
        cs.insert(self.col, c);
        self.lines[self.line] = cs.into_iter().collect();
        self.col += 1;
        self.dirty = true;
    }

    fn insert_newline(&mut self) {
        self.clamp();
        let cs = self.chars(self.line);
        let head: String = cs[..self.col].iter().collect();
        let tail: String = cs[self.col..].iter().collect();
        self.lines[self.line] = head;
        self.lines.insert(self.line + 1, tail);
        self.line += 1;
        self.col = 0;
        self.dirty = true;
    }

    /// Backspace: inside a line it removes the char before the cursor; at
    /// column 0 it joins this line onto the end of the one above.
    fn backspace(&mut self) {
        self.clamp();
        if self.col > 0 {
            let mut cs = self.chars(self.line);
            cs.remove(self.col - 1);
            self.lines[self.line] = cs.into_iter().collect();
            self.col -= 1;
            self.dirty = true;
        } else if self.line > 0 {
            let cur = self.lines.remove(self.line);
            self.line -= 1;
            self.col = self.line_len(self.line);
            self.lines[self.line].push_str(&cur);
            self.dirty = true;
        }
    }

    /// Delete: inside a line it removes the char under the cursor; at the end
    /// of a line it pulls the next line up.
    fn delete(&mut self) {
        self.clamp();
        if self.col < self.line_len(self.line) {
            let mut cs = self.chars(self.line);
            cs.remove(self.col);
            self.lines[self.line] = cs.into_iter().collect();
            self.dirty = true;
        } else if self.line + 1 < self.lines.len() {
            let next = self.lines.remove(self.line + 1);
            self.lines[self.line].push_str(&next);
            self.dirty = true;
        }
    }

    /// Move the cursor by whole lines, keeping the column where the shorter
    /// line allows.
    fn move_line(&mut self, delta: i32) {
        self.clamp();
        let n = self.lines.len();
        self.line = step(self.line, delta, n);
        self.col = self.col.min(self.line_len(self.line));
    }

    /// Left/right by one char, wrapping over a line end the way every editor
    /// with a linear buffer does.
    fn move_col(&mut self, right: bool) {
        self.clamp();
        if right {
            if self.col < self.line_len(self.line) {
                self.col += 1;
            } else if self.line + 1 < self.lines.len() {
                self.line += 1;
                self.col = 0;
            }
        } else if self.col > 0 {
            self.col -= 1;
        } else if self.line > 0 {
            self.line -= 1;
            self.col = self.line_len(self.line);
        }
    }

    /// Keep the cursor inside the window the last paint reported. With no
    /// paint yet (`rows == 0`) the scroll is left alone rather than guessed.
    pub fn follow_cursor(&mut self, rows: usize) {
        if rows == 0 {
            return;
        }
        if self.line < self.scroll {
            self.scroll = self.line;
        } else if self.line >= self.scroll + rows {
            self.scroll = self.line + 1 - rows;
        }
    }
}

/// Everything [`super::code_git::save_file`] needs, as one value.
///
/// The page never writes: it hands this to the shell, which is the only half
/// with a filesystem. Carrying the terminator and the trailing-newline flag
/// *in the request* rather than letting the shell re-derive them is the whole
/// CRLF guarantee — the shape came off the file when it was read, travelled
/// with the buffer, and goes back down unaltered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SaveRequest {
    pub rel: String,
    pub lines: Vec<String>,
    pub ending: LineEnding,
    pub trailing_newline: bool,
    /// The hash the buffer was opened with. `save_file` refuses if the file on
    /// disk no longer hashes to it.
    pub expect: u64,
}

/// The chat strip's one input line, and a pointer to where the last answer
/// went.
///
/// It deliberately does **not** hold a transcript. The answer arrives in the
/// main transcript like every other one, and a second copy of that renderer
/// here would be two views of one conversation that can disagree; the strip
/// says which question went out and where to read the answer.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Strip {
    pub input: String,
    /// Cursor position in **chars** of [`Self::input`], `0..=len`. Chars for
    /// the same reason [`OpenFile::col`] is: this indexes the string, and the
    /// paint is the half that converts to display columns.
    pub cursor: usize,
    /// The last question submitted from here, for the pointer row. It is the
    /// person's words and not the composed line, because the composed line is
    /// mostly their own file read back at them.
    pub sent: Option<String>,
}

impl Strip {
    /// The input as chars, which is the unit the cursor is in.
    fn chars(&self) -> Vec<char> {
        self.input.chars().collect()
    }

    fn insert(&mut self, c: char) {
        // The same door `OpenFile::insert_char` guards, for the same reason:
        // this string is composed into a line that leaves the program, and a
        // control byte in it is a control byte in somebody's transcript.
        if c.is_control() {
            return;
        }
        let mut cs = self.chars();
        let at = self.cursor.min(cs.len());
        cs.insert(at, c);
        self.input = cs.into_iter().collect();
        self.cursor = at + 1;
    }

    fn backspace(&mut self) {
        let mut cs = self.chars();
        if self.cursor == 0 || cs.is_empty() {
            return;
        }
        let at = self.cursor.min(cs.len());
        cs.remove(at - 1);
        self.input = cs.into_iter().collect();
        self.cursor = at - 1;
    }

    fn delete(&mut self) {
        let mut cs = self.chars();
        if self.cursor >= cs.len() {
            return;
        }
        cs.remove(self.cursor);
        self.input = cs.into_iter().collect();
    }

    /// The question, or `None` when there is nothing but whitespace in the
    /// box. Empties the box only when it returns something, so a stray Enter
    /// cannot throw away a line somebody typed.
    fn take(&mut self) -> Option<String> {
        let q = self.input.trim().to_string();
        if q.is_empty() {
            return None;
        }
        self.input.clear();
        self.cursor = 0;
        Some(q)
    }
}

/// The most file body that rides along with a question from the chat strip.
///
/// The price of this feature is tokens and it is paid on every question: 16 KB
/// is roughly four thousand tokens of context, a real cost but a bounded one.
/// Past it the whole file is not sent — the visible window is, **named as a
/// line range**, so the answer cannot quietly be about a part emma never saw.
/// Past even that, only the path goes and the line says so.
pub const MAX_ASK_BODY: usize = 16 * 1024;

/// Compose the line the chat strip submits: an attribution naming exactly what
/// is included, the text itself, then the person's question — one line a
/// person could have typed.
///
/// **The attribution is not decoration.** An answer about a file emma was
/// never shown is the failure this exists to prevent, so the header always
/// says what went, including when that is nothing but a path, when it is only
/// a line range, when it carries unsaved edits, and when the bytes are not the
/// file's own ([`SANITISED_NOTE`] — the buffer is what the page could draw,
/// and saying otherwise would be fabricating agreement with a file on disk).
///
/// `rows` is the height of the document pane, so the window is what the person
/// can see. Pure: it reads the buffer the page already holds and never a file.
pub fn compose_ask(open: &OpenFile, rows: usize, question: &str) -> String {
    let n = open.lines.len();
    let mut caveat = String::new();
    if open.dirty {
        // The unsaved edits *are* what goes, because the buffer is the file
        // being asked about. Saying so beats a surprise in either direction.
        caveat.push_str("; with unsaved edits");
    }
    if open.locked == Some(SANITISED_NOTE) {
        caveat.push_str(
            "; tabs and control bytes were replaced to draw it, so this is not \
                         byte-for-byte the file",
        );
    }
    let path = &open.path;
    if n == 0 {
        return format!("About `{path}` (the file is empty):\n\n{question}");
    }
    let whole = open.lines.join("\n");
    if whole.len() <= MAX_ASK_BODY {
        return format!(
            "About `{path}` (the whole file, {n} lines{caveat}):\n\n```\n{whole}\n```\n\n{question}"
        );
    }
    let top = open.scroll.min(n - 1);
    let end = (top + rows).min(n);
    let window = open.lines[top..end].join("\n");
    if end > top && window.len() <= MAX_ASK_BODY {
        return format!(
            "About `{path}` (lines {a}-{b} of {n}; the rest is too large to include{caveat}):\
             \n\n```\n{window}\n```\n\n{question}",
            a = top + 1,
            b = end,
        );
    }
    format!(
        "About `{path}` ({n} lines; too large to include any of it here, read it if you need \
         it{caveat}):\n\n{question}"
    )
}

/// A file's history and the selected commit's diff. `None` on the view until
/// HISTORY is first entered for the open file; the shell fills it then, and
/// the pane says "loading history…" in the meantime.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct History {
    pub commits: Vec<Commit>,
    pub sel: usize,
    pub scroll: usize,
    pub diff: Vec<DiffRow>,
    pub diff_scroll: usize,
    /// The commit `diff` belongs to. A late answer for a commit that is no
    /// longer selected is dropped against this — with the fetch on a worker
    /// thread, two requests can be in flight and the slower one must not
    /// overwrite the faster one's patch with a stale one.
    pub diff_for: Option<String>,
    /// Honest empty state: a file with no history, rather than an empty pane.
    pub note: Option<String>,
    /// The diff stage. `false` is the commit list over the whole region;
    /// `true` is the selected commit's patch over the whole region.
    pub showing_diff: bool,
}

/// Everything the Code page draws.
#[derive(Debug, Clone)]
pub struct CodeView {
    /// The repository root — the run's cwd. Every relative path resolves
    /// here, and it is what [`CodeAction::LaunchEditor`] hands the editor.
    pub root: PathBuf,
    pub nodes: Vec<Node>,
    pub expanded: BTreeSet<String>,
    /// Selection as an index into the *visible* rows, not into [`Self::nodes`].
    pub tree_sel: usize,
    pub tree_scroll: usize,
    pub open: Option<OpenFile>,
    pub mode: Mode,
    pub history: Option<History>,
    pub focus: Focus,
    /// One line under the header: a save receipt, a refusal, or what the page
    /// just declined to do. Cleared by the next key that acts.
    pub notice: Option<String>,
    /// Whether the document has the keyboard as an *editor* rather than as a
    /// viewer. Explicit, and not implied by [`Focus::Body`], because stage a
    /// shipped a body that scrolls with `j`/`k` and a pane that silently
    /// started taking letters instead would be the worst kind of surprise:
    /// the one that edits a file.
    pub edit: bool,
    /// What an armed unsaved-changes warning will do if the next key confirms
    /// it. The first press warns, the second acts, and anything else cancels
    /// without also being acted on.
    pub armed: Option<Pending>,
    /// How many document rows the last paint had room for. Only the paint
    /// knows it, so it is stored here for PageUp/PageDown and for keeping the
    /// cursor on screen.
    pub body_rows: usize,
    /// The chat strip: what is typed into it, and what was last sent.
    pub strip: Strip,
    /// Whether the last paint had room to draw the strip at all — under twelve
    /// inner rows the document keeps them. Only the paint knows the height, and
    /// Tab must not reach a box nobody can see, so the shell stores the answer
    /// here beside [`Self::body_rows`], from [`Regions::strip`].
    ///
    /// It starts **true**, which is the safe default in the one direction that
    /// matters: an unwired shell leaves a reachable strip on every terminal
    /// tall enough to draw it, where the alternative would be a feature that
    /// silently does not exist.
    pub strip_shown: bool,
    /// Everything a language server has told the page. Never written by a key:
    /// only by an [`LspUpdate`] the bridge in [`super::code_lsp`] delivered,
    /// through [`CodeView::apply_lsp`]. With no bridge wired it stays
    /// [`LspStatus::Idle`] and nothing on the page changes, which is the same
    /// state a file with no language server leaves it in.
    pub lsp: Lsp,
}

/// The action an unsaved-changes warning is holding back.
///
/// Only the two things that actually drop a buffer are here. Entering HISTORY
/// is **not** one of them and the fork's version guarded it anyway: `Mode`
/// changes what the right pane draws and leaves `open` alone, so the edits are
/// still there on the way back. A warning about a loss that cannot happen is
/// how people learn to press through warnings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Pending {
    /// Close the page. The buffer goes with it.
    Close,
    /// Open another file over this one.
    Open(String),
}

/// Rows a wheel notch moves the body. Three is what terminals scroll by, and
/// what [`super::input::WHEEL_ROWS`] already uses for the transcript.
const WHEEL_ROWS: i32 = 3;

impl CodeView {
    /// A page over `root`, with the tree already flattened.
    pub fn new(root: PathBuf, nodes: Vec<Node>) -> Self {
        Self {
            root,
            nodes,
            expanded: BTreeSet::new(),
            tree_sel: 0,
            tree_scroll: 0,
            open: None,
            mode: Mode::File,
            history: None,
            focus: Focus::Tree,
            notice: None,
            edit: false,
            armed: None,
            body_rows: 0,
            strip: Strip::default(),
            strip_shown: true,
            lsp: Lsp::default(),
        }
    }

    /// Whether keys are going into the document right now, which is what
    /// decides between the editor's key map and the browser's.
    ///
    /// Every clause is load-bearing: HISTORY does not draw the document,
    /// another focused region owns the keys, the person has not asked to edit,
    /// or the file cannot be written back — in any of those, a letter is a
    /// browser shortcut and not text.
    pub fn editing(&self) -> bool {
        self.edit
            && self.mode == Mode::File
            && self.focus == Focus::Body
            && self.open.as_ref().is_some_and(OpenFile::editable)
    }

    /// Whether the open buffer has edits that are not on disk.
    pub fn dirty(&self) -> bool {
        self.open.as_ref().is_some_and(|o| o.dirty)
    }

    /// The indices into [`Self::nodes`] whose every ancestor directory is
    /// expanded — exactly the rows the tree draws, in order.
    pub fn visible(&self) -> Vec<usize> {
        self.nodes
            .iter()
            .enumerate()
            .filter(|(_, n)| self.ancestors_expanded(&n.path))
            .map(|(i, _)| i)
            .collect()
    }

    fn ancestors_expanded(&self, path: &str) -> bool {
        let parts: Vec<&str> = path.split('/').collect();
        for k in 1..parts.len() {
            if !self.expanded.contains(&parts[..k].join("/")) {
                return false;
            }
        }
        true
    }

    /// The node the selection points at, or `None` for an empty tree.
    pub fn selected_node(&self) -> Option<&Node> {
        let vis = self.visible();
        vis.get(self.tree_sel).map(|&i| &self.nodes[i])
    }

    /// The shell's answer to [`CodeAction::Open`]: the file it read, made
    /// safe to draw. Opening a file drops the history that belonged to the
    /// last one, which is the only thing keeping HISTORY honest about *which*
    /// file it is the history of.
    /// `disk_hash` is [`super::code_git::file_hash`] over the same path — see
    /// [`OpenFile::from_read`] for why the page wants both answers.
    pub fn set_open(&mut self, path: String, read: FileRead, disk_hash: Option<u64>) {
        self.open = Some(OpenFile::from_read(path, read, disk_hash));
        self.history = None;
        self.mode = Mode::File;
        self.focus = Focus::Body;
        // A new file is a new document: the viewer, not the editor. Somebody
        // who wants to type says so again, on the file they are looking at.
        self.edit = false;
        // The pointer row names a question about the file that was open when
        // it was asked. Left standing over a different file it would be a
        // sentence about the wrong thing, which is the whole class `History`
        // is dropped here for. What is half-typed in the box is the person's
        // and stays.
        self.strip.sent = None;
        // Every answer about the *previous* file goes with it, and the popup is
        // the one that could do damage: it inserts into the buffer, so one left
        // standing across a file switch would put the old file's candidate into
        // the new file's text. The hover, the panel and the colours are only
        // wrong on screen, which is reason enough on its own.
        self.lsp.popup = None;
        self.lsp.places = None;
        self.lsp.hover = None;
        self.lsp.tokens.clear();
        self.lsp.tokens_for = None;
    }

    /// The shell's answer to [`CodeAction::Save`].
    ///
    /// Nothing is assumed about what happened: `Saved::Ok` carries the hash of
    /// the bytes actually written and that becomes the buffer's new baseline,
    /// so a second save compares against what is on disk rather than against
    /// what was there when the file was opened. The other two arms leave the
    /// buffer dirty, because it still is.
    pub fn set_saved(&mut self, saved: Saved) {
        match saved {
            Saved::Ok(hash) => {
                if let Some(o) = self.open.as_mut() {
                    o.hash = Some(hash);
                    o.dirty = false;
                }
                self.notice = Some("saved".to_string());
            }
            Saved::ChangedOnDisk => {
                self.notice = Some(
                    "not saved: this file changed on disk since it was opened — reopen it to \
                     see what changed"
                        .to_string(),
                );
            }
            Saved::Refused(why) => self.notice = Some(format!("not saved: {why}")),
        }
    }

    /// What the shell just sent to the clipboard.
    ///
    /// **Sent, and never arrived.** OSC 52 has no acknowledgement and `pbcopy`
    /// is a process away, so the only honest sentence is about the bytes that
    /// left — the same wording `/copy` and the mouse selection already use.
    pub fn notice_sent(&mut self, chars: usize) {
        self.notice = Some(format!(
            "sent {chars} character(s) to the clipboard — if nothing pasted, this terminal \
             refuses the sequence"
        ));
    }

    /// Clipboard text arriving from the terminal's bracketed paste.
    ///
    /// Only into an editable document with the keyboard: a paste over the tree
    /// or the commit list has nowhere to land, and pretending otherwise would
    /// drop the text silently. The refusal is a notice, not silence.
    pub fn paste_text(&mut self, text: &str) -> CodeAction {
        if !self.editing() {
            self.notice = Some("nothing here takes a paste — Enter on the file first".to_string());
            return CodeAction::FocusChanged;
        }
        let rows = self.body_rows;
        let Some(o) = self.open.as_mut() else {
            return CodeAction::None;
        };
        let changed = o.paste(text);
        o.follow_cursor(rows);
        if changed {
            // Said, not merely done — `input::PasteLoss`'s rule, applied to
            // the one other box in this program that takes a paste.
            self.notice =
                Some("pasted with changes: tabs became spaces, control bytes dropped".to_string());
        }
        CodeAction::FocusChanged
    }

    /// The shell's answer to [`CodeAction::LoadHistory`]. Returns the hash of
    /// the commit whose diff should be fetched next, or `None` when there is
    /// no history to show — so the caller never has to guess which commit the
    /// page selected.
    pub fn set_history(&mut self, commits: Vec<Commit>, note: Option<String>) -> Option<String> {
        let first = commits.first().map(|c| c.hash.clone());
        let note = match (&note, commits.is_empty()) {
            (Some(_), _) => note,
            (None, true) => Some("no history for this file".to_string()),
            (None, false) => None,
        };
        self.history = Some(History {
            commits,
            note,
            ..History::default()
        });
        first
    }

    /// The shell's answer to [`CodeAction::LoadDiff`]. A patch for a commit
    /// that is no longer the selected one is **dropped**, not drawn: with the
    /// fetch on a worker thread the answers can arrive out of order, and a
    /// patch under the wrong commit's name is worse than a slow one.
    /// Answers whether it was taken.
    pub fn set_diff(&mut self, hash: &str, rows: Vec<DiffRow>) -> bool {
        let Some(h) = self.history.as_mut() else {
            return false;
        };
        if h.commits.get(h.sel).map(|c| c.hash.as_str()) != Some(hash) {
            return false;
        }
        h.diff = rows;
        h.diff_for = Some(hash.to_string());
        h.diff_scroll = 0;
        true
    }

    /// A wheel notch over the page. The body is what scrolls — the tree keeps
    /// its selection, because a wheel that moved a selection would open
    /// files nobody pointed at. Returns an action for the same reason a key
    /// does: over the commit list a notch changes which commit is selected,
    /// and that needs a patch fetched.
    pub fn wheel(&mut self, up: bool) -> CodeAction {
        let delta = if up { -WHEEL_ROWS } else { WHEEL_ROWS };
        match self.mode {
            Mode::File => {
                let Some(o) = self.open.as_mut() else {
                    return CodeAction::None;
                };
                o.scroll = step_scroll(o.scroll, delta);
                CodeAction::FocusChanged
            }
            Mode::History => {
                if self.history.as_ref().is_some_and(|h| h.showing_diff) {
                    if let Some(h) = self.history.as_mut() {
                        h.diff_scroll = step_scroll(h.diff_scroll, delta);
                    }
                    return CodeAction::FocusChanged;
                }
                self.select_commit(delta)
            }
        }
    }
}

/// Build the flattened tree from a list of repo-relative file paths.
/// Directories sort before files at each level, alphabetically, in DFS order,
/// so a plain path-prefix test decides visibility with no child graph to keep
/// in sync.
pub fn build_nodes(paths: &[String]) -> Vec<Node> {
    use std::collections::BTreeMap;

    #[derive(Default)]
    struct Dir {
        subdirs: BTreeMap<String, Dir>,
        files: BTreeSet<String>,
    }

    let mut root = Dir::default();
    for p in paths {
        let parts: Vec<&str> = p.split('/').filter(|s| !s.is_empty()).collect();
        if parts.is_empty() {
            continue;
        }
        let mut cur = &mut root;
        for (i, part) in parts.iter().enumerate() {
            if i + 1 == parts.len() {
                cur.files.insert((*part).to_string());
            } else {
                cur = cur.subdirs.entry((*part).to_string()).or_default();
            }
        }
    }

    fn dfs(dir: &Dir, prefix: &str, depth: usize, out: &mut Vec<Node>) {
        for (name, sub) in &dir.subdirs {
            let path = join(prefix, name);
            out.push(Node {
                path: path.clone(),
                name: name.clone(),
                depth,
                is_dir: true,
            });
            dfs(sub, &path, depth + 1, out);
        }
        for name in &dir.files {
            out.push(Node {
                path: join(prefix, name),
                name: name.clone(),
                depth,
                is_dir: false,
            });
        }
    }

    let mut out = Vec::new();
    dfs(&root, "", 0, &mut out);
    out
}

fn join(prefix: &str, name: &str) -> String {
    if prefix.is_empty() {
        name.to_string()
    } else {
        format!("{prefix}/{name}")
    }
}

fn parent_of(path: &str) -> Option<String> {
    path.rfind('/').map(|i| path[..i].to_string())
}

// endregion: State

// region: LSP state
// ---------------------------------------------------------------------------
// LSP state: what the language server told the page, and nothing else
// ---------------------------------------------------------------------------
//
// Every field here is written by an [`LspUpdate`] that arrived from the bridge
// task in [`super::code_lsp`], and read by the paint. Nothing in this region
// talks to a server, waits on one, or knows one exists: the page is a display
// of the last thing it was told, which is what lets a dead or slow server cost
// the editor its decorations and never a keystroke.
// ---------------------------------------------------------------------------

/// How bad one diagnostic is, in the four grades LSP defines.
///
/// Ordered worst-first on purpose: [`Lsp::at_line`] picks the gutter mark with
/// `min_by_key`, so a line carrying an error and a hint shows the error.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Error,
    Warning,
    Info,
    Hint,
}

impl Severity {
    /// LSP's `DiagnosticSeverity`. Anything else is a `Hint`, because an
    /// unknown grade is still a thing the server wanted to say and dropping it
    /// would be the page inventing silence.
    pub fn from_lsp(n: i64) -> Self {
        match n {
            1 => Severity::Error,
            2 => Severity::Warning,
            3 => Severity::Info,
            _ => Severity::Hint,
        }
    }

    /// The gutter glyph.
    ///
    /// **One display column, ASCII, and that is a load-bearing constraint
    /// rather than a taste.** The mark is drawn into the gutter's separator
    /// column — the space between the line number and the text — so that the
    /// gutter width [`geom_for`] hands the hit test does not move when a
    /// diagnostic arrives. A two-column glyph there would make the row one
    /// column wider than the pane, and [`put`] would clip the last character
    /// of every marked line. [`Severity::mark`] is pinned by a test that draws
    /// a wide glyph at the right edge and looks for it.
    pub fn mark(self) -> char {
        match self {
            Severity::Error => 'E',
            Severity::Warning => 'W',
            Severity::Info => 'i',
            Severity::Hint => 'h',
        }
    }

    fn role(self) -> Role {
        match self {
            Severity::Error => Role::Err,
            Severity::Warning => Role::Warn,
            Severity::Info | Severity::Hint => Role::Dim,
        }
    }
}

/// One diagnostic, in this page's coordinates: 0-based lines and **chars**,
/// which is what [`OpenFile`] indexes by.
///
/// The conversion from LSP's UTF-16 columns happens once, in
/// [`super::code_lsp`], against the buffer the server was told about. Doing it
/// here would mean the paint converting encodings on every frame; doing it
/// nowhere would put the underline under the wrong character on any line with
/// a non-ASCII glyph on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diag {
    pub line: usize,
    pub end_line: usize,
    /// First char of the span on [`Self::line`].
    pub start_col: usize,
    /// One past the last char of the span on [`Self::end_line`].
    pub end_col: usize,
    pub severity: Severity,
    pub message: String,
}

/// What the page can honestly say about the language server for the open file.
///
/// The vocabulary is the Settings LSP card's, deliberately: **found** means an
/// entry point is on disk, **running** means a process answered a handshake,
/// and the two are different claims. Nothing here is ever inferred from a
/// decoration arriving; every variant is something the bridge observed.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum LspStatus {
    /// No file open, so there is no question to answer.
    #[default]
    Idle,
    /// This file's extension names no language in `emma_tools_lsp::lang`.
    Unsupported(String),
    /// A language Emma knows, switched off in `lsp.enabled`.
    Disabled(String),
    /// Enabled, and no entry point on disk. The detail is the crate's own
    /// refusal, which names what was looked for.
    Absent { label: String, detail: String },
    /// A server is being started, or is still indexing. Not yet answering.
    Starting(String),
    /// A handshake completed and the server is answering.
    Running(String),
    /// It was running and stopped, or it never started. The pool's crash
    /// accounting decides whether another one is tried.
    Failed(String),
}

impl LspStatus {
    /// The one line the page shows about the server. Present tense, no
    /// promises: a caller can print this beside a file and be right.
    pub fn line(&self) -> String {
        match self {
            LspStatus::Idle => String::new(),
            LspStatus::Unsupported(shown) => format!("no language server for {shown}"),
            LspStatus::Disabled(label) => {
                format!("{label} is off: lsp.enabled in settings.json is the opt-in")
            }
            LspStatus::Absent { label, detail } => format!("{label}: {detail}"),
            LspStatus::Starting(what) => format!("{what} starting"),
            LspStatus::Running(what) => format!("{what} running"),
            LspStatus::Failed(why) => format!("language server stopped: {why}"),
        }
    }

    fn role(&self) -> Role {
        match self {
            LspStatus::Running(_) => Role::Ok,
            LspStatus::Failed(_) | LspStatus::Absent { .. } => Role::Warn,
            _ => Role::Dim,
        }
    }
}

/// The hover answer, as a popup with its own scroll.
///
/// Bounded by construction: [`HOVER_MAX_LINES`] is applied where the popup is
/// built, so an enormous doc comment cannot become a page-sized overlay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HoverPopup {
    pub lines: Vec<String>,
    pub scroll: usize,
}

/// One thing that could be typed here.
///
/// A narrowed `emma_tools_lsp::render::Completion`: the page keeps what it
/// draws and what it inserts, and drops the rest at the boundary rather than
/// carrying protocol shapes into the view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// What the list shows.
    pub label: String,
    /// What typing is matched against. Not the label: a server labels a method
    /// `push(…)` and filters it as `push`, and matching the label is why some
    /// editors stop finding anything after the bracket.
    pub filter: String,
    /// What goes into the buffer.
    pub insert: String,
    /// The word this replaces, as a column range on the cursor's line, when the
    /// server named one. `None` leaves the page to decide, which it does by
    /// taking the identifier before the cursor.
    pub replace: Option<(usize, usize)>,
    /// A word, or empty when the server sent a kind this build does not name.
    pub kind: &'static str,
    /// The type or path shown beside the label.
    pub detail: Option<String>,
}

impl Diag {
    /// Whether this diagnostic touches a line at all.
    ///
    /// The gutter mark asked this one way round and the underline asked it the
    /// other, De Morgan'd, in two different functions. One rule, and the next
    /// person to change what a multi-line span means has one place to change
    /// it.
    pub fn spans_line(&self, line: usize) -> bool {
        line >= self.line && line <= self.end_line
    }
}

/// The completion popup: what came back, what has been typed since, and which
/// row is chosen.
///
/// **`typed` is the whole reason this is a struct rather than a list.** A
/// server answers about one position; the person then keeps typing. Re-asking
/// on every keystroke is what makes an editor feel slow, so the popup narrows
/// its own list against the characters typed since it opened, and only asks
/// again when the answer it holds was marked incomplete.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Popup {
    /// Everything the server offered, in the server's own ranking.
    pub items: Vec<Candidate>,
    /// What has been typed since the request went out, and **only** that.
    ///
    /// It is empty at the moment an answer lands, so the list is first drawn
    /// unnarrowed. That is right for a server which filters by the prefix
    /// itself — rust-analyzer does — and wrong for one that returns the whole
    /// set in scope, which would then be shown in full until the next
    /// keystroke. Seeding this with the identifier before the cursor would fix
    /// that case; it is not done here, because the fix cannot be certified
    /// against a server nobody on this project has run, and a wrongly seeded
    /// filter hides candidates with no visible way to reach them. The doc used
    /// to claim the seeding was already happening.
    pub typed: String,
    /// The chosen row, as an index into [`Self::visible`].
    pub at: usize,
    /// The server truncated its own list and expects to be asked again.
    pub incomplete: bool,
    /// The line and column the request was made at, so a cursor that has moved
    /// off that line closes the popup rather than inserting somewhere else.
    pub origin: (usize, usize),
}

/// The most rows the popup draws. A glance, like the hover popup: the file
/// underneath is what the page is for, and a list longer than this is one
/// nobody reads to the end.
pub const POPUP_MAX_ROWS: usize = 8;

impl Popup {
    /// The items that match what has been typed, best first.
    ///
    /// Case-insensitive prefix first, then a case-insensitive substring, which
    /// is the behaviour people expect from every editor they have used. The
    /// server's ranking is preserved inside each group rather than re-sorted:
    /// it knows which of two matches is more likely and this does not.
    pub fn visible(&self) -> Vec<&Candidate> {
        if self.typed.is_empty() {
            return self.items.iter().collect();
        }
        let needle = self.typed.to_lowercase();
        let mut prefix = Vec::new();
        let mut contains = Vec::new();
        for item in &self.items {
            let hay = item.filter.to_lowercase();
            if hay.starts_with(&needle) {
                prefix.push(item);
            } else if hay.contains(&needle) {
                contains.push(item);
            }
        }
        prefix.extend(contains);
        prefix
    }

    /// The chosen item, or `None` when nothing matches what has been typed.
    pub fn chosen(&self) -> Option<&Candidate> {
        let visible = self.visible();
        visible
            .get(self.at.min(visible.len().saturating_sub(1)))
            .copied()
    }

    /// Move the selection, clamped rather than wrapped: a list that jumps from
    /// the bottom to the top under a held key is one people overshoot.
    pub fn move_by(&mut self, down: bool) {
        let len = self.visible().len();
        if len == 0 {
            self.at = 0;
            return;
        }
        self.at = if down {
            (self.at + 1).min(len - 1)
        } else {
            self.at.saturating_sub(1)
        };
    }
}

/// One row of the places list: a reference, or a symbol in this file.
///
/// The two share a type because they share a purpose and a key: both answer
/// "where do I go", both draw as a path and a line, and both are opened by
/// pressing Enter on a row. Two nearly identical lists would be two places to
/// fix the next time the opening rule changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Place {
    /// What the row says, and it is **not** the line's own text: a reference
    /// answer names files the page has not read, so
    /// `code_lsp::reference_places` writes `path:line`, and `symbol_places`
    /// writes the symbol's name indented by its nesting. Neither shows a kind.
    /// This doc claimed both for a while, which is the sort of thing a reader
    /// only finds out by opening the producers — so the producers are named.
    pub label: String,
    /// Repo-relative. A place outside the root is not offered at all, which is
    /// the containment rule the definition jump already follows.
    pub rel: String,
    /// 0-based, as LSP counts and as the page indexes.
    pub line: usize,
    pub col: usize,
}

/// The places panel: references to a symbol, or the symbols in a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Places {
    /// What was asked, for the header: the symbol's name, or the file's.
    pub about: String,
    pub rows: Vec<Place>,
    pub at: usize,
}

impl Places {
    pub fn move_by(&mut self, down: bool) {
        if self.rows.is_empty() {
            return;
        }
        self.at = if down {
            (self.at + 1).min(self.rows.len() - 1)
        } else {
            self.at.saturating_sub(1)
        };
    }

    pub fn chosen(&self) -> Option<&Place> {
        self.rows.get(self.at)
    }
}

/// The most rows the places panel draws at once.
pub const PLACES_MAX_ROWS: usize = 12;

/// The most hover lines kept. rust-analyzer's hover on a trait method runs to
/// hundreds; the popup is a glance, and the file underneath is what the page
/// is for.
pub const HOVER_MAX_LINES: usize = 40;

/// Where a `textDocument/definition` answer points.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DefTarget {
    /// Inside the repository root, as a repo-relative path and a 0-based
    /// position. The column is still an LSP **UTF-16** offset here: the file
    /// it indexes into has not been read yet, so the conversion happens in the
    /// shell after the open, where the lines exist.
    Inside {
        rel: String,
        line: usize,
        col: usize,
    },
    /// Outside the root: the standard library, a registry checkout, a
    /// generated file in a build directory. The containment law says the page
    /// does not open it, so it says where it is instead.
    Outside(String),
    /// The server answered, and the answer was nothing.
    NotFound,
}

/// One thing the bridge learned, on its way to the view.
///
/// Applied by [`CodeView::apply_lsp`] under the frame lock. Every variant
/// carries the path it is about, because an answer can arrive after the person
/// has opened a different file, and decorating the new file with the old
/// file's diagnostics is the failure this field exists to prevent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LspUpdate {
    Status(LspStatus),
    Diagnostics {
        path: String,
        items: Vec<Diag>,
    },
    /// `None` is "the server had nothing to say here", which is a real answer
    /// and gets its own sentence rather than an empty popup.
    Hover {
        path: String,
        lines: Option<Vec<String>>,
    },
    Definition {
        path: String,
        target: DefTarget,
    },
    /// Where a symbol is used, or what a file contains. An empty list is a
    /// real answer and says so rather than opening an empty panel.
    Places {
        path: String,
        about: String,
        rows: Vec<Place>,
    },
    /// What every run of characters in the file is, for colour.
    Tokens {
        path: String,
        items: Vec<emma_tools_lsp::render::Token>,
    },
    /// Which characters the server says should open a list, learned from the
    /// handshake. Sent once per server rather than guessed, because a guess is
    /// wrong for every language whose server disagrees with it.
    Triggers {
        completion: Vec<String>,
        signature: Vec<String>,
    },
    /// What could be typed here. An empty list is a real answer and closes the
    /// popup rather than leaving the last one on screen.
    Completions {
        path: String,
        items: Vec<Candidate>,
        incomplete: bool,
        /// Where the request was made, so an answer that arrives after the
        /// cursor has moved off the line is dropped rather than applied to a
        /// position it was not about.
        origin: (usize, usize),
    },
    /// Which argument the cursor is in, already rendered to one line.
    Signature {
        path: String,
        line: Option<String>,
    },
    /// The bridge could not do the thing at all, in its own words.
    Note {
        path: String,
        text: String,
    },
}

/// Everything the language server contributes to the page.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Lsp {
    pub status: LspStatus,
    /// Which file [`Self::diags`] describes. `None` means the diagnostics are
    /// about nothing and are not drawn — a stale set is not dimmed, it is
    /// absent.
    pub path: Option<String>,
    pub diags: Vec<Diag>,
    pub hover: Option<HoverPopup>,
    /// The completion popup, when one is open.
    pub popup: Option<Popup>,
    /// References, or this file's symbols, when either was asked for.
    pub places: Option<Places>,
    /// What the server said each run of characters in the open file is, for
    /// colour. Empty when there is no server, which draws plain text rather
    /// than a highlighter's guess.
    pub tokens: Vec<emma_tools_lsp::render::Token>,
    /// Which file [`Self::tokens`] describes, so a stale set is dropped rather
    /// than painted over a different file.
    pub tokens_for: Option<String>,
    /// Characters that open a completion list, as the server named them.
    pub completion_triggers: Vec<String>,
    /// Characters that open signature help, as the server named them.
    pub signature_triggers: Vec<String>,
    /// Set by typing when the character warrants asking, read and cleared by
    /// the shell.
    ///
    /// **A flag rather than a call, because the page cannot reach the
    /// bridge.** The page knows what was typed and the shell owns the channel,
    /// and this is the seam between them: the same shape `code_line` uses for
    /// the chat strip.
    pub want_completion: bool,
    /// A one-line answer that is not a diagnostic: a definition outside the
    /// root, a hover with nothing in it, a request that timed out.
    pub note: Option<String>,
}

impl Lsp {
    /// Errors and warnings in the open file, for the status row's count.
    pub fn counts(&self) -> (usize, usize) {
        let errors = self
            .diags
            .iter()
            .filter(|d| d.severity == Severity::Error)
            .count();
        let warnings = self
            .diags
            .iter()
            .filter(|d| d.severity == Severity::Warning)
            .count();
        (errors, warnings)
    }

    /// `"2 errors, 1 warning"`, or `None` when there is nothing to count.
    ///
    /// Only errors and warnings are counted, and the silence about hints is
    /// deliberate: rust-analyzer emits an "inactive code" hint for every
    /// `cfg`-disabled block, and a count that included them would report
    /// dozens of problems in a file that has none.
    pub fn count_line(&self) -> Option<String> {
        let (e, w) = self.counts();
        if e == 0 && w == 0 {
            return None;
        }
        let mut parts = Vec::new();
        if e > 0 {
            parts.push(format!("{e} error{}", if e == 1 { "" } else { "s" }));
        }
        if w > 0 {
            parts.push(format!("{w} warning{}", if w == 1 { "" } else { "s" }));
        }
        Some(parts.join(", "))
    }

    /// The worst diagnostic covering a line, for the gutter mark.
    pub fn at_line(&self, line: usize) -> Option<&Diag> {
        self.diags
            .iter()
            .filter(|d| d.spans_line(line))
            .min_by_key(|d| d.severity)
    }
}

impl CodeView {
    /// Fold one answer from the bridge into the page.
    ///
    /// Returns the file the shell must open, and only that: a definition that
    /// landed in a file which is not the open one is the one answer this page
    /// cannot apply on its own, because opening a file is a read. Everything
    /// else is view state and is applied here.
    ///
    /// **Stale answers are dropped, not drawn.** A hover that arrives two
    /// seconds after the person moved to another file is about a buffer that
    /// is no longer on screen, and drawing it would be the page asserting
    /// something nobody asked about the file in front of them.
    pub fn apply_lsp(&mut self, update: LspUpdate) -> Option<(String, usize, usize)> {
        let open = self.open.as_ref().map(|o| o.path.clone());
        match update {
            LspUpdate::Status(status) => {
                self.lsp.status = status;
                None
            }
            LspUpdate::Diagnostics { path, items } => {
                if open.as_deref() == Some(path.as_str()) {
                    self.lsp.path = Some(path);
                    self.lsp.diags = items;
                }
                None
            }
            LspUpdate::Hover { path, lines } => {
                if open.as_deref() != Some(path.as_str()) {
                    return None;
                }
                match lines {
                    Some(lines) if !lines.is_empty() => {
                        self.lsp.note = None;
                        self.lsp.hover = Some(HoverPopup {
                            lines: lines.into_iter().take(HOVER_MAX_LINES).collect(),
                            scroll: 0,
                        });
                    }
                    _ => {
                        self.lsp.hover = None;
                        self.lsp.note = Some("no hover information here".to_string());
                    }
                }
                None
            }
            LspUpdate::Definition { path, target } => {
                if open.as_deref() != Some(path.as_str()) {
                    return None;
                }
                match target {
                    DefTarget::NotFound => {
                        self.lsp.note =
                            Some("no definition found for the symbol at the cursor".to_string());
                        None
                    }
                    DefTarget::Outside(shown) => {
                        // The containment law, said out loud rather than
                        // silently. Opening it would put a file the page
                        // cannot save into an editor whose Save writes inside
                        // the root.
                        self.lsp.note = Some(format!("defined outside this repository: {shown}"));
                        None
                    }
                    DefTarget::Inside { rel, line, col } => {
                        if Some(rel.as_str()) == open.as_deref() {
                            // Same file: the column is already a char column,
                            // because the shell converted it against the
                            // buffer it had. See `code_lsp::definition_target`.
                            self.jump_to(line, col);
                            self.lsp.note = Some(format!("jumped to line {}", line + 1));
                            None
                        } else {
                            Some((rel, line, col))
                        }
                    }
                }
            }
            LspUpdate::Places { path, about, rows } => {
                if open.as_deref() != Some(path.as_str()) {
                    return None;
                }
                if rows.is_empty() {
                    self.lsp.places = None;
                    self.lsp.note = Some(format!("no results for {about}"));
                    return None;
                }
                self.lsp.places = Some(Places { about, rows, at: 0 });
                None
            }
            LspUpdate::Tokens { path, items } => {
                if open.as_deref() == Some(path.as_str()) {
                    self.lsp.tokens_for = Some(path);
                    self.lsp.tokens = items;
                }
                None
            }
            LspUpdate::Triggers {
                completion,
                signature,
            } => {
                self.lsp.completion_triggers = completion;
                self.lsp.signature_triggers = signature;
                None
            }
            LspUpdate::Completions {
                path,
                items,
                incomplete,
                origin,
            } => {
                if open.as_deref() != Some(path.as_str()) {
                    return None;
                }
                // An answer about a position the cursor has left is dropped.
                // Applying it would offer members of whatever was under the
                // cursor a moment ago, at a place they do not belong.
                let here = self.open.as_ref().map(|o| (o.line, o.col));
                if here.map(|(l, _)| l) != Some(origin.0) {
                    return None;
                }
                if items.is_empty() {
                    self.lsp.popup = None;
                    self.lsp.note = Some("nothing to complete here".to_string());
                    return None;
                }
                // Whatever has been typed since the request went out narrows
                // the list immediately, so a fast typist does not see the
                // popup flash the unfiltered set.
                let typed = here
                    .map(|(_, col)| col.saturating_sub(origin.1))
                    .and_then(|extra| {
                        let o = self.open.as_ref()?;
                        let line = o.lines.get(o.line)?;
                        let chars: Vec<char> = line.chars().collect();
                        Some(chars.get(origin.1..origin.1 + extra)?.iter().collect())
                    })
                    .unwrap_or_default();
                self.lsp.popup = Some(Popup {
                    items,
                    typed,
                    at: 0,
                    incomplete,
                    origin,
                });
                None
            }
            LspUpdate::Signature { path, line } => {
                if open.as_deref() == Some(path.as_str()) {
                    // The signature shares the note row rather than opening a
                    // second popup: two floating boxes over three lines of code
                    // is a page nobody can read.
                    self.lsp.note = line;
                }
                None
            }
            LspUpdate::Note { path, text } => {
                if open.as_deref() == Some(path.as_str()) {
                    self.lsp.note = Some(text);
                }
                None
            }
        }
    }

    /// Put the cursor at a position and scroll it into view.
    pub fn jump_to(&mut self, line: usize, col: usize) {
        let rows = self.body_rows.max(1);
        if let Some(open) = self.open.as_mut() {
            open.line = line.min(open.lines.len().saturating_sub(1));
            open.col = col.min(open.line_len(open.line));
            open.anchor = None;
            open.follow_cursor(rows);
        }
    }

    /// The diagnostic under the cursor, which is what the status row shows.
    ///
    /// The path check is the same one the paint makes: a diagnostic set that
    /// is not about the open file is not about anything.
    pub fn diag_at_cursor(&self) -> Option<&Diag> {
        let open = self.open.as_ref()?;
        if self.lsp.path.as_deref() != Some(open.path.as_str()) {
            return None;
        }
        self.lsp.at_line(open.line)
    }

    /// Put the chosen completion into the buffer.
    ///
    /// The range replaced is the server's when it named one, and the identifier
    /// before the cursor when it did not. That fallback is not a guess about
    /// the language: it is the same rule every editor uses, and the server's
    /// own range is preferred precisely because it *is* language-aware.
    pub fn accept_completion(&mut self) -> CodeAction {
        let origin = self.lsp.popup.as_ref().map(|p| p.origin);
        let Some(chosen) = self.lsp.popup.as_ref().and_then(|p| p.chosen()).cloned() else {
            // Nothing matches what has been typed. Closing without inserting is
            // the honest answer; inserting the first item of a list the person
            // has typed past is how an editor writes something nobody asked for.
            self.lsp.popup = None;
            return CodeAction::FocusChanged;
        };
        self.lsp.popup = None;
        let Some(o) = self.open.as_mut() else {
            return CodeAction::FocusChanged;
        };
        let Some(line) = o.lines.get(o.line).cloned() else {
            return CodeAction::FocusChanged;
        };
        let chars: Vec<char> = line.chars().collect();
        // **The server's range is only good where the server was asked.** It
        // was computed at `origin`, and the list narrows as somebody types
        // without asking again, so by the time this runs the cursor has usually
        // moved. rust-analyzer answers a dot completion with an *empty* replace
        // range at the request position -- certified, 1.94.1 -- so applying it
        // after three more letters inserts the candidate and leaves those three
        // behind: `c.pus` accepted as `push` became `c.pushpus`. When the cursor
        // has moved, the word before it is the honest range, which is the same
        // rule every editor uses and the same fallback a server naming no range
        // already gets.
        let moved = origin != Some((o.line, o.col));
        let (from, to) = match chosen.replace.filter(|_| !moved) {
            Some((a, b)) => (a.min(chars.len()), b.min(chars.len())),
            None => (word_start(&chars, o.col), o.col.min(chars.len())),
        };
        let mut next: String = chars[..from].iter().collect();
        next.push_str(&chosen.insert);
        next.extend(chars[to..].iter());
        o.lines[o.line] = next;
        o.col = from + chosen.insert.chars().count();
        o.dirty = true;
        o.clamp();
        CodeAction::FocusChanged
    }

    /// Ask for hover or a definition, if there is a file to ask about.
    ///
    /// The refusal is the honest half: with no readable file open there is no
    /// position to ask about, and a request the shell cannot fill would come
    /// back to the person as silence.
    fn ask_lsp(&mut self, action: CodeAction) -> CodeAction {
        if self.open.as_ref().is_none_or(|o| o.note.is_some()) {
            self.lsp.note = Some("open a text file first".to_string());
            return CodeAction::FocusChanged;
        }
        self.lsp.note = None;
        action
    }

    /// Ask for a completion list, which needs one thing more than the others.
    ///
    /// **The document has to be being edited.** Every other question here
    /// answers into a note or a panel, and this one answers into a popup whose
    /// only exit is a key that `edit_key` owns: opened from the tree or from a
    /// read-only document, it covered the code and nothing dismissed it, and
    /// `Esc` closed the page out from under it. Worse, it survived opening a
    /// second file, so accepting it later inserted the first file's candidate
    /// into the second file's buffer.
    fn ask_complete(&mut self) -> CodeAction {
        if !self.editing() {
            self.lsp.note = Some("press Enter to edit before asking what can be typed".to_string());
            return CodeAction::FocusChanged;
        }
        self.ask_lsp(CodeAction::Complete)
    }
}

// endregion: LSP state

// region: Keys
// ---------------------------------------------------------------------------
// Keys — the pure seam
// ---------------------------------------------------------------------------

/// What a key asks the shell to do. Everything that needs a file, git or a
/// process crosses this enum; the handler below never touches disk, which is
/// what makes it testable without a repository.
///
/// The LSP half adds `Hover`, `Definition`, `Complete`, `References`,
/// `Symbols` and the `Goto` that opens a row of the last two.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodeAction {
    /// Not this page's key (or a chord/release): let it fall through.
    None,
    /// The view changed (selection, scroll, expand, mode) and wants a repaint.
    FocusChanged,
    /// Open this repo-relative file in the body pane.
    Open(String),
    /// Load history for the open file.
    LoadHistory,
    /// The selected commit changed; the shell fetches its patch.
    LoadDiff(String),
    /// Hand the repository to the configured external editor — the launch
    /// `Alt+c` used to be, kept as a door inside the page it became.
    LaunchEditor,
    /// Write the buffer back, through [`super::code_git::save_file`]. The
    /// answer comes back through [`CodeView::set_saved`].
    Save(SaveRequest),
    /// Put this text on the system clipboard, by whatever mechanism the frame
    /// already uses for `/copy` and for a mouse selection. One mechanism, one
    /// more caller — a second escape sequence written from here would be a
    /// second thing to get wrong on the terminals that already refuse the
    /// first.
    Copy(String),
    /// Ask what could be typed at the cursor. The answer arrives as
    /// [`LspUpdate::Completions`] and opens the popup.
    Complete,
    /// Open a row from the places panel. The column is still a UTF-16
    /// offset, because the file it indexes may not have been read yet.
    Goto(Place),
    /// Ask where the symbol under the cursor is used.
    References,
    /// Ask what this file contains.
    Symbols,
    /// A question about the open file, already composed into the line a person
    /// could have typed ([`compose_ask`]).
    ///
    /// **Composed here and not by the shell**, which is the one judgement in
    /// this variant: the buffer, its unsaved edits, the visible line range and
    /// the reason a file is locked all live on this page, and a shell that
    /// re-derived them would be the second answer to one question this module
    /// has already paid for twice. The shell's whole job is to put the string
    /// on the channel a typed line takes — so the steering queue decides what
    /// happens to it mid-goal, exactly as it does for a typed line.
    Ask(String),
    /// Ask the language server what is under the cursor (`F5`).
    ///
    /// It carries no position, and that is the seam rather than an oversight:
    /// the shell reads the cursor and the buffer off the page when it posts
    /// the request, so there is one answer to "where is the cursor" and it is
    /// [`OpenFile`]'s. A position captured here would be a second copy, taken
    /// one key earlier.
    Hover,
    /// Ask where the symbol under the cursor is defined (`F6`).
    Definition,
    /// Close the page.
    Close,
}

/// Whether the open page keeps this key at all.
///
/// **Every plain key belongs to the open page, acted on or not** — the Memory
/// and Harness pages' law, for the same reason: with a page over the
/// transcript nothing else could honestly reach it. Releases, Alt and Ctrl are
/// the exceptions, so `Alt+c` still closes the page it opened and `Ctrl-C`,
/// `Ctrl-B` and the capture toggle keep working over it.
///
/// It is public because the shell needs the same answer to decide whether to
/// hand the key on, and a second copy of the predicate in `app.rs` is the
/// "one input shape, two answers" defect this codebase has already paid for.
/// It takes the view for the same reason: the one exception below depends on
/// what the page is doing, and a predicate that could not see that would have
/// to guess.
///
/// **`Ctrl+s` is the exception, and only while a buffer is being edited.**
/// Everywhere else in the program that chord toggles mouse capture
/// ([`super::input::pane_key`]); here, with the document taking letters, a
/// capture toggle has nothing to act on and the save has no other chord that
/// every terminal delivers. `F2` does the same thing and is bound nowhere
/// else, so a terminal that eats `Ctrl+s` for flow control still has a key —
/// which is why the header names both rather than promising one.
///
/// **The chat strip did not widen this.** A second text box on the page is a
/// reason somebody might reach for a second predicate; there is none, and
/// there must not be. Every plain key was already the page's, so the strip
/// needs nothing added — it is [`handle_key`]'s last layer that decides which
/// box a letter lands in, and it can only be reached through here. The one
/// Ctrl chord still answers on [`CodeView::editing`], which is false while the
/// strip has the keyboard: `Ctrl+s` over the strip is the capture toggle it is
/// over the tree, and `F2` is the save key that works from anywhere on the
/// page. A test pins that pair, because the tempting change is to make the
/// strip "also" take `Ctrl+s` and that is how the two predicates start.
pub fn takes_key(v: &CodeView, key: KeyEvent) -> bool {
    if key.kind == KeyEventKind::Release || key.modifiers.contains(KeyModifiers::ALT) {
        return false;
    }
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        // **Two chords now, and the second was advertised for a fortnight
        // without ever arriving.** `Ctrl+space` is named on the help row and in
        // the module header, and this predicate returned `false` for it, so the
        // completion arm in `handle_key` was unreachable code and the key did
        // nothing at all -- the exact "drawn control that does nothing" the
        // page refuses everywhere else. It is admitted here and answered above
        // the save chord, because the blanket `Ctrl` arm below that would
        // otherwise *save the file* when somebody asked for a completion.
        return key.code == KeyCode::Char(' ') || (key.code == KeyCode::Char('s') && v.editing());
    }
    true
}

/// One key, against the whole page.
///
/// The layers, in this order, because each can only be reached by getting past
/// the one above it. **The list is unnumbered on purpose**: it used to say
/// "five layers" over six of them, and by the time the overlays were added it
/// was a count nobody had recounted.
///
/// - **[`takes_key`]** — releases and Alt are never ours, and neither is Ctrl
///   but for the save chord.
/// - **The save chord**, above the warning below it, because saving is the way
///   *out* of an unsaved-changes warning and must not be the key that cancels
///   one.
/// - **An armed warning**, which consumes the key that answers it. **Above the
///   overlays, and that is a fix rather than a preference.** A places answer
///   can land while a warning is armed — press `F8` on an edited buffer, then
///   `Esc` — and with the panel above the warning the panel ate the key that
///   would have cancelled it, leaving the warning armed with its notice on
///   screen so the *next* matching key confirmed a discard. A destructive
///   action must never be reachable by a key somebody thought was closing a
///   list.
/// - **The overlays**, places before hover. Each takes every key that reaches
///   it, the accent picker's rule: it covers the text, and a key that fell
///   through to the editor while a panel hid the line would edit a line the
///   person cannot see. **Places first, because that is what the paint draws.**
///   Both can be set at once — `F8` then `F5` before the references answer
///   lands — and `draw_body` resolves that in favour of the panel, so the
///   keyboard has to resolve it the same way. It did not, and the first key
///   after that pair silently closed an invisible hover.
/// - **The function keys**, which work from anywhere on the page — including
///   from inside the document, where every letter is text. That is the whole
///   reason they are function keys.
/// - **The chat strip**, when it has the keyboard — and only then, which is
///   the whole of "the strip must not eat a key the editor needs": the strip
///   is one branch of one layer, not a rule beside the layers.
/// - The editor, when the document is being edited, or the browser otherwise.
pub fn handle_key(v: &mut CodeView, key: KeyEvent) -> CodeAction {
    if !takes_key(v, key) {
        return CodeAction::None;
    }
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        // `takes_key` lets two chords through, and this one must be answered
        // before the save: an unqualified `Ctrl` arm would turn a request for a
        // completion into a write to disk.
        if key.code == KeyCode::Char(' ') {
            return v.ask_complete();
        }
        return v.request_save();
    }
    if key.code == KeyCode::F(2) {
        return v.request_save();
    }
    // Above the overlays: a panel that ate the answer to this would leave the
    // warning armed and its notice on screen, and the next matching key would
    // confirm a discard nobody had just asked for. See the layer list above.
    if let Some(pending) = v.armed.take() {
        v.notice = None;
        // The same key again confirms; anything else cancels and is **not**
        // also acted on, so a discard can never be reached by one keystroke.
        return if confirms(&pending, key.code) {
            v.discard(pending)
        } else {
            CodeAction::FocusChanged
        };
    }
    // The places panel first, because `draw_body` draws it over the hover: the
    // two can both be set, and whichever is on screen is the one that owns the
    // arrows. Unlike the completion popup it is a *destination*, so Enter
    // belongs to it rather than to the buffer underneath.
    if v.lsp.places.is_some() {
        return places_key(v, key);
    }
    // The hover popup takes every key that reaches it, for the same reason. It
    // sits below the save chord deliberately, so `Ctrl+s` and `F2` still save
    // with a popup open.
    if v.lsp.hover.is_some() {
        return hover_key(v, key);
    }
    match key.code {
        KeyCode::F(3) => return v.toggle_mode(),
        KeyCode::F(4) => return v.copy(),
        // The second door (owner ruling D1, 2026-09-06). A function key
        // because it has to keep working with the document taking letters,
        // and because the header names it beside the `[Editor]` button that
        // does the same thing.
        KeyCode::F(7) => return CodeAction::LaunchEditor,
        // The two code-intelligence keys. Function keys for the reason `F3`
        // and `F4` are: inside the document every letter is text, so a
        // mnemonic letter could not reach them. Both work from the tree too,
        // so a person who has not focused the body still has them — and both
        // refuse in words when there is no readable file to ask about.
        KeyCode::F(5) => return v.ask_lsp(CodeAction::Hover),
        KeyCode::F(6) => return v.ask_lsp(CodeAction::Definition),
        // The two list answers. `F8` and `F10` rather than the pair after
        // `F6`, because `F7` already opens the outside editor and `F9` already
        // asks for a completion, and moving either to keep these adjacent
        // would change a key somebody has already learned.
        KeyCode::F(8) => return v.ask_lsp(CodeAction::References),
        KeyCode::F(10) => return v.ask_lsp(CodeAction::Symbols),
        // **Two spellings, because one of them does not survive every
        // terminal.** `Ctrl+Space` is the chord every editor uses and some
        // terminals swallow it; `F9` is the escape hatch that always arrives.
        // Both are drawn on the help row, so nobody has to discover the second
        // after the first appeared to do nothing.
        KeyCode::F(9) => return v.ask_complete(),
        _ => {}
    }
    v.notice = None;
    if v.focus == Focus::Chat {
        return strip_key(v, key);
    }
    if v.editing() {
        return edit_key(v, key);
    }
    browse_key(v, key)
}

/// The chat strip's keys: a one-line box that asks about the open file.
///
/// Every key here is the one a person expects from a one-line box, and the two
/// that are not obvious are the ones worth the comment. `Esc` leaves the strip
/// rather than the page — C2's argument about the editor, unchanged: the key
/// that leaves a text box must not also be the key that throws away what is in
/// it, and here what is in it is a half-typed question. `Enter` on an empty
/// box does **nothing at all** rather than sending a question with no words:
/// a fabricated question is a fabricated answer.
fn strip_key(v: &mut CodeView, key: KeyEvent) -> CodeAction {
    match key.code {
        KeyCode::Esc => v.leave_strip(),
        KeyCode::Tab => {
            v.cycle_focus();
            CodeAction::FocusChanged
        }
        KeyCode::Enter => match v.strip.take() {
            Some(q) => v.ask(&q),
            None => CodeAction::None,
        },
        KeyCode::Backspace => {
            v.strip.backspace();
            CodeAction::FocusChanged
        }
        KeyCode::Delete => {
            v.strip.delete();
            CodeAction::FocusChanged
        }
        KeyCode::Left => {
            v.strip.cursor = v.strip.cursor.saturating_sub(1);
            CodeAction::FocusChanged
        }
        KeyCode::Right => {
            v.strip.cursor = (v.strip.cursor + 1).min(v.strip.input.chars().count());
            CodeAction::FocusChanged
        }
        KeyCode::Home => {
            v.strip.cursor = 0;
            CodeAction::FocusChanged
        }
        KeyCode::End => {
            v.strip.cursor = v.strip.input.chars().count();
            CodeAction::FocusChanged
        }
        KeyCode::Char(c) => {
            v.strip.insert(c);
            CodeAction::FocusChanged
        }
        _ => CodeAction::None,
    }
}

/// The hover popup's keys. Every one of them is consumed: the popup is modal
/// by design, and the two that do something are the arrows. Anything else
/// closes it **without also acting**, the same rule the unsaved-changes
/// warning follows — a key that dismisses an overlay and then does its usual
/// job is one keystroke doing two things the person only asked for one of.
fn hover_key(v: &mut CodeView, key: KeyEvent) -> CodeAction {
    let Some(popup) = v.lsp.hover.as_mut() else {
        return CodeAction::None;
    };
    match key.code {
        KeyCode::Down => popup.scroll = (popup.scroll + 1).min(popup.lines.len().saturating_sub(1)),
        KeyCode::Up => popup.scroll = popup.scroll.saturating_sub(1),
        _ => v.lsp.hover = None,
    }
    CodeAction::FocusChanged
}

/// The keys the places panel owns while it is open.
///
/// It takes **every** key, the hover popup's rule rather than the completion
/// popup's: this panel covers the code and is a list somebody is reading, so a
/// key falling through would edit a line hidden behind it. Enter opens the
/// chosen row and any other key closes the panel, which makes leaving it the
/// cheapest thing on the page.
///
/// `None` for the returned action when the row names the file already open:
/// the jump is a cursor move this side can make, and nothing needs to be read.
fn places_key(v: &mut CodeView, key: KeyEvent) -> CodeAction {
    match key.code {
        KeyCode::Up | KeyCode::Down => {
            if let Some(places) = v.lsp.places.as_mut() {
                places.move_by(key.code == KeyCode::Down);
            }
            CodeAction::FocusChanged
        }
        KeyCode::Enter => match v.lsp.places.take() {
            Some(p) => match p.rows.get(p.at) {
                Some(row) => CodeAction::Goto(row.clone()),
                None => CodeAction::FocusChanged,
            },
            None => CodeAction::FocusChanged,
        },
        _ => {
            v.lsp.places = None;
            CodeAction::FocusChanged
        }
    }
}

/// Whether this key confirms a pending discard. It is the key that asked for
/// it: pressing the same thing twice is the gesture, and every other key
/// cancels.
fn confirms(p: &Pending, code: KeyCode) -> bool {
    match p {
        Pending::Close => code == KeyCode::Esc,
        Pending::Open(_) => code == KeyCode::Enter,
    }
}

/// What the server said this character is, as a colour role.
///
/// A linear scan of **one line's** tokens, which is what [`tokens_on_line`]
/// hands it: a line has a handful, and a map built per repaint would cost more
/// than it saves. `Role::Text` when nothing covers the character, which is both
/// the answer for ordinary identifiers and the answer when there is no server.
fn syntax_role(tokens: &[emma_tools_lsp::render::Token], col: usize) -> Role {
    use emma_tools_lsp::render::TokenKind;
    let Some(token) = tokens.iter().find(|t| col >= t.start && col < t.end) else {
        return Role::Text;
    };
    match token.kind {
        TokenKind::Comment => Role::Comment,
        TokenKind::Keyword => Role::Keyword,
        TokenKind::Str => Role::Str,
        TokenKind::Number => Role::Number,
        TokenKind::Type => Role::Type,
        TokenKind::Func => Role::Func,
        TokenKind::Other => Role::Text,
    }
}

/// The run of tokens that belong to one line.
///
/// **Two binary searches rather than a filter, and the reason is a measurement
/// rather than a preference.** `parse_tokens` returns the answer in the order
/// the wire sent it, which is line order, so the tokens for a line are a
/// contiguous run. Before this the painter asked every token in the file about
/// every cell it drew: 28,330 tokens for this file against roughly five
/// thousand cells, on every repaint, which is every keystroke.
///
/// An empty slice for a line with nothing on it, and for every line when there
/// is no server — the same answer, which is what makes the caller's plain-text
/// fallback one branch instead of two.
fn tokens_on_line(
    tokens: &[emma_tools_lsp::render::Token],
    line: usize,
) -> &[emma_tools_lsp::render::Token] {
    let start = tokens.partition_point(|t| t.line < line);
    let end = start + tokens[start..].partition_point(|t| t.line == line);
    &tokens[start..end]
}

/// Whether typing `c` should ask for a completion list.
///
/// **Two reasons to ask, and they are different questions.** A trigger
/// character is the server's own claim that something follows it: a dot in
/// Rust, a colon, an open bracket. Those open a list immediately, with nothing
/// typed, because the useful answer is the whole set of members. An identifier
/// character opens one only once there are enough letters to narrow it, because
/// a list of everything in scope after one letter is a list nobody reads.
///
/// The word is measured **after** the typed character has gone in, so
/// `MIN_WORD` of 3 means the list appears on the third letter. Later than a
/// graphical editor, deliberately: there a suggestion list floats over its own
/// layer, and here it covers the code being written, so it has to earn the
/// rows. Three is also far enough in that the request is cheap while still
/// ahead of the person.
fn should_complete(v: &CodeView, c: char) -> bool {
    if v.lsp
        .completion_triggers
        .iter()
        .any(|t| t == &c.to_string())
    {
        return true;
    }
    if !(c.is_alphanumeric() || c == '_') {
        return false;
    }
    // Already open: typing narrows what is there rather than asking again,
    // unless the server truncated its own answer and asked to be re-queried.
    if let Some(popup) = v.lsp.popup.as_ref() {
        return popup.incomplete;
    }
    let Some(o) = v.open.as_ref() else {
        return false;
    };
    let Some(line) = o.lines.get(o.line) else {
        return false;
    };
    let chars: Vec<char> = line.chars().collect();
    o.col.saturating_sub(word_start(&chars, o.col)) >= MIN_WORD
}

/// How long the word must be, counting the character just typed, before a list
/// opens by itself.
pub const MIN_WORD: usize = 3;

/// Where the identifier under the cursor starts.
///
/// The fallback when a server names no range, and deliberately the dullest
/// possible rule: letters, digits and underscore. Every language this build
/// speaks agrees about those, and a cleverer rule would be a second opinion
/// about syntax in a crate whose whole point is asking the server instead.
fn word_start(chars: &[char], col: usize) -> usize {
    let mut at = col.min(chars.len());
    while at > 0 && (chars[at - 1].is_alphanumeric() || chars[at - 1] == '_') {
        at -= 1;
    }
    at
}

/// The keys the completion popup owns while it is open, and only those.
///
/// **Everything else falls through to the editor**, which is the rule that
/// keeps this from being the feature people turn off. A popup that swallowed
/// Enter would stop somebody adding a line; one that swallowed Backspace would
/// strand them. So this answers for exactly six keys and refuses the rest, and
/// typing a character both inserts it and narrows the list.
///
/// `None` means the popup did not take the key.
fn popup_key(v: &mut CodeView, key: KeyEvent) -> Option<CodeAction> {
    let popup = v.lsp.popup.as_mut()?;
    match key.code {
        KeyCode::Esc => {
            v.lsp.popup = None;
            Some(CodeAction::FocusChanged)
        }
        KeyCode::Up => {
            popup.move_by(false);
            Some(CodeAction::FocusChanged)
        }
        KeyCode::Down => {
            popup.move_by(true);
            Some(CodeAction::FocusChanged)
        }
        // Both accept, because both are what people press. Tab is the habit
        // from every editor; Enter is what somebody who has never used one
        // tries. Neither reaches the buffer as a character while the popup is
        // up, which is the one thing this layer takes away.
        KeyCode::Tab | KeyCode::Enter => Some(v.accept_completion()),
        _ => None,
    }
}

/// The document's keys. Every printable character is text, which is why the
/// browser's `j`/`k`/`g` shortcuts are not reachable from here: `Tab` cycles
/// the panes and that is where they live, and `Esc` puts the document back to
/// being read.
///
/// Shift with an arrow extends a selection; an arrow without it drops one.
/// Typing, Enter, Backspace and Delete all replace a live selection, which is
/// what every editor does and what makes a selection worth having.
fn edit_key(v: &mut CodeView, key: KeyEvent) -> CodeAction {
    // The popup first, and it takes six keys. See `popup_key`.
    if let Some(action) = popup_key(v, key) {
        return action;
    }
    let rows = v.body_rows;
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    match key.code {
        // Out of the editor, not out of the page and not out of the edits:
        // the buffer keeps them, `Esc` again asks about the page. Stage a's
        // `Esc`-closes-the-page is one press further away and that is the
        // point — the key that leaves a text box should not also be the key
        // that throws the text away.
        KeyCode::Esc => return v.leave_edit(),
        KeyCode::Tab => {
            v.cycle_focus();
            return CodeAction::FocusChanged;
        }
        _ => {}
    }
    // Set by the character arm below, applied after the buffer borrow ends.
    let mut narrow: Option<char> = None;
    // The character typed, so the trigger decision can be made once the borrow
    // is over and the whole view is readable again.
    let mut ask = '\0';
    let Some(o) = v.open.as_mut() else {
        return CodeAction::None;
    };
    match key.code {
        KeyCode::Up => {
            o.mark(shift);
            o.move_line(-1);
        }
        KeyCode::Down => {
            o.mark(shift);
            o.move_line(1);
        }
        KeyCode::Left => {
            o.mark(shift);
            o.move_col(false);
        }
        KeyCode::Right => {
            o.mark(shift);
            o.move_col(true);
        }
        KeyCode::Home => {
            o.mark(shift);
            o.col = 0;
        }
        KeyCode::End => {
            o.mark(shift);
            o.col = o.line_len(o.line);
        }
        KeyCode::PageUp => {
            o.mark(shift);
            o.move_line(-(rows.max(1) as i32));
        }
        KeyCode::PageDown => {
            o.mark(shift);
            o.move_line(rows.max(1) as i32);
        }
        KeyCode::Enter => {
            o.delete_selection();
            o.insert_newline();
        }
        KeyCode::Backspace => {
            if !o.delete_selection() {
                o.backspace();
            }
        }
        KeyCode::Delete => {
            if !o.delete_selection() {
                o.delete();
            }
        }
        // A literal tab is not insertable, and that is deliberate rather than
        // missing: `Tab` is the key that cycles the panes, and a TAB in the
        // buffer is a character this page has already decided it cannot draw
        // honestly. Spaces are what it can keep.
        KeyCode::Char(c) => {
            o.delete_selection();
            o.insert_char(c);
            // The popup narrows against what has been typed since it opened,
            // rather than re-asking the server on every keystroke. Re-asking is
            // what makes an editor feel slow; the exception is a list the
            // server marked incomplete, which it truncated and expects to be
            // asked about again.
            narrow = Some(c);
            ask = c;
        }
        _ => return CodeAction::None,
    }
    o.follow_cursor(rows);
    // After the buffer borrow ends. A typed character narrows an open popup,
    // and closes it once nothing matches: a list showing items that do not
    // contain what is on screen is worse than no list.
    if let Some(c) = narrow {
        let gone = match v.lsp.popup.as_mut() {
            Some(popup) => {
                popup.typed.push(c);
                popup.at = 0;
                popup.visible().is_empty()
            }
            None => false,
        };
        if gone {
            v.lsp.popup = None;
        }
    }
    if ask != '\0' && should_complete(v, ask) {
        // The shell reads and clears this; the page cannot reach the channel.
        v.lsp.want_completion = true;
    }
    CodeAction::FocusChanged
}

/// The browser's keys: the tree, the commit list, and the patch.
fn browse_key(v: &mut CodeView, key: KeyEvent) -> CodeAction {
    match key.code {
        KeyCode::Esc => v.back(),
        // `b` is the way back out of a patch, and only that: from the file
        // view it would be a page close nobody asked for, and Esc already is.
        KeyCode::Char('b') if v.mode == Mode::History => v.back(),
        KeyCode::Tab => {
            v.cycle_focus();
            CodeAction::FocusChanged
        }
        // `g` and `H` both switch tabs. `H` is what the header advertises,
        // because "HISTORY" starts with it; `g` was the original binding and
        // keeping it costs nothing. The tabs are clickable too, which is the
        // answer to "there is no way to hit history".
        KeyCode::Char('g') | KeyCode::Char('H') => v.toggle_mode(),
        // Bare `h` is the tree's collapse key (vi's left), so it only means
        // HISTORY where there is no tree under the cursor.
        KeyCode::Char('h') if v.focus != Focus::Tree => v.toggle_mode(),
        KeyCode::Up | KeyCode::Char('k') => v.move_by(-1),
        KeyCode::Down | KeyCode::Char('j') => v.move_by(1),
        KeyCode::Right | KeyCode::Char('l') => v.expand_or_in(),
        KeyCode::Left | KeyCode::Char('h') => v.collapse_or_out(),
        KeyCode::Enter => v.activate(),
        KeyCode::PageUp => v.page(true),
        KeyCode::PageDown => v.page(false),
        _ => CodeAction::None,
    }
}

impl CodeView {
    /// Esc and `b`, one step out at a time: the patch returns to the commit
    /// list, the commit list to the file, and the file closes the page.
    fn back(&mut self) -> CodeAction {
        match self.mode {
            Mode::History => {
                let showing = self.history.as_ref().is_some_and(|h| h.showing_diff);
                if showing {
                    if let Some(h) = self.history.as_mut() {
                        h.showing_diff = false;
                    }
                    self.focus = Focus::Commits;
                } else {
                    self.mode = Mode::File;
                    self.focus = Focus::Body;
                }
                CodeAction::FocusChanged
            }
            Mode::File => self.guard(Pending::Close),
        }
    }

    /// Tab, round the regions the current mode has, with the chat strip last
    /// in both — it is what a person reaches after looking at the thing they
    /// want to ask about.
    ///
    /// The strip is skipped when the paint had no room for it
    /// ([`Self::strip_shown`]). A focus on a box that is not on screen is
    /// typing into nothing, which is worse than not having the box.
    fn cycle_focus(&mut self) {
        self.focus = match (self.mode, self.focus) {
            (Mode::File, Focus::Tree) => Focus::Body,
            (Mode::File, Focus::Body) if self.strip_shown => Focus::Chat,
            (Mode::File, _) => Focus::Tree,
            (Mode::History, Focus::Tree) => Focus::Commits,
            (Mode::History, Focus::Commits) => Focus::Diff,
            (Mode::History, Focus::Diff) if self.strip_shown => Focus::Chat,
            (Mode::History, _) => Focus::Tree,
        };
    }

    /// `Esc` out of the strip, back to the region Tab arrived from. Not
    /// [`Self::cycle_focus`], which would carry on to the tree: leaving a text
    /// box should put the keyboard back where it was, not one step further on.
    fn leave_strip(&mut self) -> CodeAction {
        self.focus = match self.mode {
            Mode::File => Focus::Body,
            Mode::History => Focus::Diff,
        };
        CodeAction::FocusChanged
    }

    /// A submitted question: compose it against the open buffer, or say why
    /// there is nothing to ask about.
    ///
    /// Both refusals are worded for the same reason every other refusal on
    /// this page is: a box that takes a question, empties itself and sends
    /// nothing is how somebody finds out ten minutes later.
    pub fn ask(&mut self, question: &str) -> CodeAction {
        let rows = self.body_rows;
        let Some(open) = self.open.as_ref() else {
            self.notice = Some("open a file to ask about it".to_string());
            return CodeAction::FocusChanged;
        };
        if open.note.is_some() {
            self.notice = Some("this file cannot be read, so it cannot be asked about".to_string());
            return CodeAction::FocusChanged;
        }
        let line = compose_ask(open, rows, question);
        let dirty = open.dirty;
        self.strip.sent = Some(question.to_string());
        self.notice = Some(if dirty {
            "sent, with your unsaved edits included — the answer is in the transcript".to_string()
        } else {
            "sent: the answer is in the transcript".to_string()
        });
        CodeAction::Ask(line)
    }

    /// FILE ↔ HISTORY.
    ///
    /// History is about a file, so with none open the key **says so** rather
    /// than doing nothing: a key that is advertised in the header and silently
    /// declines is the defect this repository keeps paying for.
    fn toggle_mode(&mut self) -> CodeAction {
        match self.mode {
            Mode::File => {
                if self.open.is_none() {
                    self.notice = Some("open a file first — history is one file's".to_string());
                    return CodeAction::FocusChanged;
                }
                self.enter_history()
            }
            Mode::History => {
                self.mode = Mode::File;
                self.focus = Focus::Body;
                CodeAction::FocusChanged
            }
        }
    }

    fn enter_history(&mut self) -> CodeAction {
        self.mode = Mode::History;
        self.focus = Focus::Commits;
        if let Some(h) = self.history.as_mut() {
            h.showing_diff = false;
        }
        if self.history.is_none() {
            return CodeAction::LoadHistory;
        }
        CodeAction::FocusChanged
    }

    fn move_by(&mut self, delta: i32) -> CodeAction {
        match self.focus {
            Focus::Tree => {
                let n = self.visible().len();
                self.tree_sel = step(self.tree_sel, delta, n);
                CodeAction::FocusChanged
            }
            Focus::Body => {
                if let Some(o) = self.open.as_mut() {
                    o.scroll = step_scroll(o.scroll, delta);
                }
                CodeAction::FocusChanged
            }
            Focus::Commits => {
                // In the patch stage the arrows scroll the patch: the commit
                // list is not on screen, so moving its selection would move
                // something nobody can see.
                if self.history.as_ref().is_some_and(|h| h.showing_diff) {
                    if let Some(h) = self.history.as_mut() {
                        h.diff_scroll = step_scroll(h.diff_scroll, delta);
                    }
                    return CodeAction::FocusChanged;
                }
                self.select_commit(delta)
            }
            Focus::Diff => {
                if let Some(h) = self.history.as_mut() {
                    h.diff_scroll = step_scroll(h.diff_scroll, delta);
                }
                CodeAction::FocusChanged
            }
            // Not reachable today: `strip_key` owns every arrow while the
            // strip has the keyboard, and the wheel does not come through
            // here. It answers rather than panicking because an exhaustive
            // match that says `unreachable!()` is a promise about callers this
            // module cannot keep — and one line has nothing to move in anyway.
            Focus::Chat => CodeAction::None,
        }
    }

    fn select_commit(&mut self, delta: i32) -> CodeAction {
        let Some(h) = self.history.as_mut() else {
            return CodeAction::None;
        };
        if h.commits.is_empty() {
            return CodeAction::None;
        }
        let new = step(h.sel, delta, h.commits.len());
        if new == h.sel {
            return CodeAction::FocusChanged;
        }
        h.sel = new;
        // The patch on screen belongs to the commit that just stopped being
        // selected. Clearing it is what makes the pane say "loading" instead
        // of showing one commit's changes under another's name.
        h.diff.clear();
        h.diff_for = None;
        h.diff_scroll = 0;
        CodeAction::LoadDiff(h.commits[new].hash.clone())
    }

    fn expand_or_in(&mut self) -> CodeAction {
        if self.focus != Focus::Tree {
            return CodeAction::None;
        }
        let Some(node) = self.selected_node().cloned() else {
            return CodeAction::None;
        };
        if node.is_dir {
            if self.expanded.contains(&node.path) {
                let n = self.visible().len();
                self.tree_sel = step(self.tree_sel, 1, n);
            } else {
                self.expanded.insert(node.path);
            }
            CodeAction::FocusChanged
        } else {
            CodeAction::None
        }
    }

    fn collapse_or_out(&mut self) -> CodeAction {
        if self.focus != Focus::Tree {
            return CodeAction::None;
        }
        let Some(node) = self.selected_node().cloned() else {
            return CodeAction::None;
        };
        if node.is_dir && self.expanded.contains(&node.path) {
            self.expanded.remove(&node.path);
        } else if let Some(parent) = parent_of(&node.path) {
            let vis = self.visible();
            if let Some(pos) = vis.iter().position(|&i| self.nodes[i].path == parent) {
                self.tree_sel = pos;
            }
        }
        CodeAction::FocusChanged
    }

    fn activate(&mut self) -> CodeAction {
        // Enter on a commit is the "select one and see the diff" step: the
        // patch takes the whole viewer region, and `b`/Esc comes back.
        if self.mode == Mode::History && self.focus == Focus::Commits {
            let Some(h) = self.history.as_mut() else {
                return CodeAction::None;
            };
            if h.commits.is_empty() {
                return CodeAction::None;
            }
            h.showing_diff = true;
            let hash = h.commits[h.sel].hash.clone();
            if h.diff_for.as_deref() == Some(hash.as_str()) {
                // Already fetched for this commit — do not shell out again.
                return CodeAction::FocusChanged;
            }
            h.diff_scroll = 0;
            return CodeAction::LoadDiff(hash);
        }
        // Enter on the document is the way in to the editor. One rule for the
        // whole page: Enter acts on whatever has the keyboard — a directory
        // opens, a file opens, a commit shows its patch, and the file already
        // open becomes editable.
        if self.mode == Mode::File && self.focus == Focus::Body {
            return self.begin_edit();
        }
        if self.focus != Focus::Tree {
            return CodeAction::None;
        }
        let Some(node) = self.selected_node().cloned() else {
            return CodeAction::None;
        };
        if node.is_dir {
            if self.expanded.contains(&node.path) {
                self.expanded.remove(&node.path);
            } else {
                self.expanded.insert(node.path);
            }
            CodeAction::FocusChanged
        } else {
            self.guard(Pending::Open(node.path))
        }
    }

    /// Turn the viewer into an editor, or say why it will not.
    ///
    /// The refusal is the whole point of the key existing: a person who has
    /// pressed Enter on a file and can then type is owed the reason when they
    /// cannot, and `OpenFile::locked` is that reason in the words the header
    /// is already showing.
    fn begin_edit(&mut self) -> CodeAction {
        match self.open.as_ref() {
            None => {
                self.notice = Some("open a file first".to_string());
                CodeAction::FocusChanged
            }
            Some(o) if o.note.is_some() => {
                self.notice = Some("there is no text here to edit".to_string());
                CodeAction::FocusChanged
            }
            Some(o) if !o.editable() => {
                let why = o.locked.unwrap_or(NOT_EXACT_NOTE);
                self.notice = Some(format!("read-only: {why}"));
                CodeAction::FocusChanged
            }
            Some(_) => {
                self.edit = true;
                // The cursor starts on the first line the viewer is showing,
                // not at the top of a file somebody has scrolled away from.
                let rows = self.body_rows;
                if let Some(o) = self.open.as_mut() {
                    o.line = o.scroll.min(o.lines.len().saturating_sub(1));
                    o.col = 0;
                    o.anchor = None;
                    o.follow_cursor(rows);
                }
                CodeAction::FocusChanged
            }
        }
    }

    /// Back to the viewer. The edits stay in the buffer, and the notice says
    /// so — leaving a text box is not a decision about the text in it.
    fn leave_edit(&mut self) -> CodeAction {
        self.edit = false;
        if let Some(o) = self.open.as_mut() {
            o.anchor = None;
        }
        if self.dirty() {
            self.notice = Some("read-only again; the unsaved changes are still here".to_string());
        }
        CodeAction::FocusChanged
    }

    /// `Ctrl+s`, `F2` and the header's `[Save]`: ask the shell to write the
    /// buffer, or say why it will not be asked.
    ///
    /// Every refusal is worded, because a save key that does nothing is how
    /// somebody finds out their work is gone at the wrong moment.
    pub fn request_save(&mut self) -> CodeAction {
        self.armed = None;
        let Some(o) = self.open.as_ref() else {
            self.notice = Some("no file open to save".to_string());
            return CodeAction::FocusChanged;
        };
        if !o.editable() {
            let why = o.locked.unwrap_or("there is no text here");
            self.notice = Some(format!("not saved: {why}"));
            return CodeAction::FocusChanged;
        }
        if !o.dirty {
            self.notice = Some("no changes to save".to_string());
            return CodeAction::FocusChanged;
        }
        self.notice = None;
        CodeAction::Save(SaveRequest {
            rel: o.path.clone(),
            lines: o.lines.clone(),
            ending: o.ending,
            trailing_newline: o.trailing_newline,
            expect: o.hash.expect("editable() proved the hash is there"),
        })
    }

    /// `F4` and the header's `[Copy]`: the selection if there is one, and the
    /// whole file otherwise.
    ///
    /// One key with the obvious meaning rather than two keys nobody can keep
    /// straight. The fork copied the whole file always, which made a selection
    /// something only a mouse could ever act on.
    pub fn copy(&mut self) -> CodeAction {
        let Some(o) = self.open.as_ref() else {
            self.notice = Some("no file open to copy".to_string());
            return CodeAction::FocusChanged;
        };
        if o.note.is_some() || o.lines.is_empty() {
            self.notice = Some("there is no text to copy".to_string());
            return CodeAction::FocusChanged;
        }
        let selected = o.selected_text();
        let text = if selected.is_empty() {
            o.whole_text()
        } else {
            selected
        };
        CodeAction::Copy(text)
    }

    /// The end of a mouse drag over the document. Nothing selected is
    /// [`CodeAction::None`], not an empty clipboard write.
    pub fn copy_selection(&mut self) -> CodeAction {
        match self.open.as_ref().map(OpenFile::selected_text) {
            Some(text) if !text.is_empty() => CodeAction::Copy(text),
            _ => CodeAction::None,
        }
    }

    /// A press in the document: the cursor goes here and a selection starts.
    /// It does **not** start editing — a click is how somebody selects text to
    /// copy, and a page that began taking letters on a click would be the
    /// surprise [`Self::edit`] exists to prevent.
    pub fn press_doc(&mut self, line: usize, col: usize) {
        self.focus = Focus::Body;
        self.notice = None;
        if let Some(o) = self.open.as_mut() {
            o.line = line;
            o.col = col;
            o.anchor = Some((line, col));
            o.clamp();
        }
    }

    /// A drag: the anchor stays, the cursor moves.
    pub fn drag_doc(&mut self, line: usize, col: usize) {
        let rows = self.body_rows;
        if let Some(o) = self.open.as_mut() {
            if o.anchor.is_none() {
                o.anchor = Some((o.line, o.col));
            }
            o.line = line;
            o.col = col;
            o.clamp();
            o.follow_cursor(rows);
        }
    }

    /// Anything that would drop an edited buffer goes through here: the first
    /// press warns and holds the action, the second one runs it.
    fn guard(&mut self, pending: Pending) -> CodeAction {
        if !self.dirty() {
            return self.run(pending);
        }
        self.notice = Some(WARN_UNSAVED.to_string());
        self.armed = Some(pending);
        CodeAction::FocusChanged
    }

    /// The confirmed half of [`Self::guard`]: throw the edits away and go.
    fn discard(&mut self, pending: Pending) -> CodeAction {
        if let Some(o) = self.open.as_mut() {
            o.dirty = false;
        }
        self.run(pending)
    }

    fn run(&mut self, pending: Pending) -> CodeAction {
        match pending {
            Pending::Close => CodeAction::Close,
            Pending::Open(rel) => CodeAction::Open(rel),
        }
    }

    fn page(&mut self, up: bool) -> CodeAction {
        let rows = self.body_rows.max(1) as i32;
        self.move_by(if up { -rows } else { rows })
    }
}

/// The unsaved-changes warning, shown once before anything can drop a buffer.
/// It names both save keys, because the whole reason there are two is that
/// some terminals eat one of them.
pub const WARN_UNSAVED: &str =
    "unsaved changes: Ctrl+s or F2 saves — press the same key again to discard";

/// Move an index by `delta`, clamped to `0..len`.
fn step(cur: usize, delta: i32, len: usize) -> usize {
    if len == 0 {
        return 0;
    }
    let max = (len - 1) as i32;
    (cur as i32 + delta).clamp(0, max) as usize
}

/// Move a scroll offset by `delta`, never below zero. The upper bound is the
/// renderer's, which knows the pane height; here we only refuse to go
/// negative.
fn step_scroll(cur: usize, delta: i32) -> usize {
    (cur as i32 + delta).max(0) as usize
}

// endregion: Keys

// region: Layout
// ---------------------------------------------------------------------------
// Layout — one pure split both the paint and the hit test read
// ---------------------------------------------------------------------------

/// The regions the page paints into.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Regions {
    pub tree: Rect,
    /// The whole right-hand pane: header, notice, and the file or the patch
    /// under them.
    pub body: Rect,
    /// The `FILE` and `HISTORY` labels. They are the hit rects a click is
    /// tested against, which is the answer to "there is no way to hit
    /// history".
    pub file_tab: Option<Rect>,
    pub history_tab: Option<Rect>,
    /// The `[Editor]` button — the second door to the external editor.
    pub editor: Option<Rect>,
    /// The `[Copy]` button.
    pub copy: Option<Rect>,
    /// The `[Save]` button.
    pub save: Option<Rect>,
    /// The chat strip's **three rows**, or `None` when the pane was too short
    /// to draw the strip at all. The shell stores `is_some()` on
    /// [`CodeView::strip_shown`] so Tab does not reach a box nobody can see.
    ///
    /// The whole block and not just the input row, because this is also the
    /// rect a test asks "did the document stop above the strip?" — and the
    /// answer to that has to be about every row the strip owns, not the last
    /// of them. The input row is `strip.bottom() - 1`.
    pub strip: Option<Rect>,
    /// Where the hover popup landed, when one is open. Reported for the reason
    /// every other rect here is: a thing on screen whose position nothing
    /// reported cannot be tested, and a test that reads the whole buffer
    /// instead cannot say the popup was *over the document* rather than
    /// somewhere in the pane.
    pub hover: Option<Rect>,
    /// How many rows the content region has. The key handler stores it on the
    /// view so PageDown moves by a screen; only the paint can know it.
    pub rows: usize,
}

/// Split `r.main` into the EXPLORER column and the body.
///
/// **The body takes every row that is left.** The design's first draft gave
/// the bottom third to a read-only copy of the chat transcript, which put
/// twenty lines of file above a pane of dead space; the transcript is one Esc
/// away and the file is what this page is for.
pub fn split(area: Rect) -> Regions {
    if area.width < 8 || area.height < 3 {
        return Regions {
            tree: area,
            body: Rect::new(area.x, area.y, 0, 0),
            ..Regions::default()
        };
    }
    let tree_w = (area.width * 28 / 100)
        .clamp(16, 46)
        .min(area.width.saturating_sub(24))
        .max(1);
    Regions {
        tree: Rect::new(area.x, area.y, tree_w, area.height),
        body: Rect::new(area.x + tree_w, area.y, area.width - tree_w, area.height),
        ..Regions::default()
    }
}

/// The `FILE` and `HISTORY` labels and the width they occupy together.
const FILE_LABEL: &str = "FILE";
const HISTORY_LABEL: &str = "HISTORY";
const MODE_W: u16 = 14; // cols("FILE · HISTORY")

/// The header's buttons, laid out right to left from the tabs. Each appears
/// only when the pane is wide enough for it *and* everything to its right, so
/// a narrow pane loses `[Save]` before it loses the tabs — which is why
/// [`header_bar`] walks leftwards rather than measuring the row once.
const EDITOR_LABEL: &str = "[Editor]";
const COPY_LABEL: &str = "[Copy]";
const SAVE_LABEL: &str = "[Save]";

/// Every control in the header row, laid out once so the paint and the hit
/// test cannot disagree about where anything is. Right-aligned, and each
/// control appears only when the pane is wide enough to hold it and
/// everything to its right.
#[derive(Debug, Clone, Copy, Default)]
struct HeaderBar {
    file_tab: Option<Rect>,
    history_tab: Option<Rect>,
    editor: Option<Rect>,
    copy: Option<Rect>,
    save: Option<Rect>,
}

/// `savable` is whether a `[Save]` would do anything. A control that cannot
/// act is not drawn dim, it is not drawn: ruling 4's "the caller shows words,
/// never a control that does nothing", applied to the one button on this page
/// that a locked buffer makes meaningless. It also buys the header seven
/// columns for the lock's own words, which is the half a reader needs.
/// Whether the open buffer could be written at all.
fn savable(v: &CodeView) -> bool {
    v.open.as_ref().is_some_and(OpenFile::editable)
}

fn header_bar(inner: Rect, savable: bool) -> HeaderBar {
    let mut bar = HeaderBar::default();
    if inner.width <= MODE_W {
        return bar;
    }
    let mode_x = inner.right() - MODE_W;
    bar.file_tab = Some(Rect::new(mode_x, inner.y, FILE_LABEL.len() as u16, 1));
    bar.history_tab = Some(Rect::new(
        mode_x + MODE_W - HISTORY_LABEL.len() as u16,
        inner.y,
        HISTORY_LABEL.len() as u16,
        1,
    ));
    // Leftwards from the tabs, each button gated on the width it and
    // everything right of it needs, plus a column of title to its left so the
    // row never reads as one run of brackets.
    let mut x = mode_x;
    let mut used = MODE_W;
    for (label, slot) in [
        (EDITOR_LABEL, 0usize),
        (COPY_LABEL, 1usize),
        (SAVE_LABEL, 2usize),
    ] {
        if slot == 2 && !savable {
            break;
        }
        let w = label.len() as u16;
        if inner.width < used + w + 2 {
            break;
        }
        x -= w + 1;
        used += w + 1;
        let rect = Some(Rect::new(x, inner.y, w, 1));
        match slot {
            0 => bar.editor = rect,
            1 => bar.copy = rect,
            _ => bar.save = rect,
        }
    }
    bar
}

/// How many rows the help line under the content takes: one, when there is
/// enough body left that spending a row on it is not stealing the file's.
fn help_rows(inner_height: u16, y: u16) -> u16 {
    u16::from(inner_height.saturating_sub(y) >= 3)
}

/// How many rows the chat strip takes off the bottom of the body: a rule, the
/// pointer row, and the input — or none at all.
///
/// **The floor is the file's, not the strip's.** Under twelve inner rows the
/// document needs every line it has more than the strip does, and three rows
/// of chat over nine rows of source is a page that has stopped being a code
/// viewer. The alternative — drawing a squeezed one-row strip — would put a
/// text box on screen with no room to show what was typed into it.
fn strip_rows(inner_height: u16) -> u16 {
    if inner_height >= 12 {
        3
    } else {
        0
    }
}

/// The rows the file or the patch is drawn into, derived from the page's own
/// area and state.
///
/// Pure, and shared by the paint and the hit test for the reason [`split`] is:
/// a geometry the paint records and the hit test re-derives is two answers to
/// one question, and they drift the first time either changes.
fn content_rect(body: Rect, v: &CodeView) -> Rect {
    let inner = Block::bordered().inner(body);
    if inner.width == 0 || inner.height == 0 {
        return Rect::new(inner.x, inner.y, 0, 0);
    }
    let y = 1 + u16::from(v.notice.is_some());
    let strip_h = strip_rows(inner.height);
    let lsp_h = lsp_rows(v);
    let help_h = help_rows(inner.height, y + strip_h + lsp_h);
    let h = inner.height.saturating_sub(y + help_h + strip_h + lsp_h);
    Rect::new(inner.x, inner.y + y, inner.width, h)
}

/// The one row the language server gets, or nothing at all when it has nothing
/// to say.
///
/// Worst first, joined with the separator the status bar uses: whatever the
/// last request had to report, then the count, then the server's state, then
/// the diagnostic under the cursor. **One row**, because the file is what the
/// pane is for — a problems panel would take a third of the page to restate
/// what the gutter already marks.
pub fn lsp_line(v: &CodeView) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    let status = v.lsp.status.line();
    if !status.is_empty() {
        parts.push(status);
    }
    if let Some(counts) = v.lsp.count_line() {
        parts.push(counts);
    }
    if let Some(note) = &v.lsp.note {
        parts.push(note.clone());
    }
    if let Some(d) = v.diag_at_cursor() {
        parts.push(format!("{}: {}", d.severity.mark(), one_line(&d.message)));
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" \u{b7} "))
    }
}

/// A message flattened to one row. rust-analyzer's diagnostics routinely carry
/// a second line, and a newline written into a `Buffer` is a cell, not a break.
fn one_line(text: &str) -> String {
    text.split('\n')
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Whether the body gives up a row for [`lsp_line`]. Read by [`content_rect`]
/// and by the paint, which must agree or a click lands on the wrong line.
fn lsp_rows(v: &CodeView) -> u16 {
    u16::from(lsp_line(v).is_some())
}

/// Where the chat strip's three rows land, or `None` when the pane is too
/// short for them. Pure and shared by the paint and the hit test, the reason
/// [`content_rect`] is: a rect the paint records and the hit test re-derives is
/// two answers to one question.
fn strip_rect(body: Rect) -> Option<Rect> {
    let inner = Block::bordered().inner(body);
    if inner.width == 0 {
        return None;
    }
    let h = strip_rows(inner.height);
    if h == 0 {
        return None;
    }
    Some(Rect::new(inner.x, inner.bottom() - h, inner.width, h))
}

/// Where a paint puts the document text, so a screen cell can be turned back
/// into a `(line, char)` position in the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DocGeom {
    /// The whole text region, gutter included.
    pub area: Rect,
    /// Width of the line-number gutter plus its separator column.
    pub gutter: u16,
    /// The first document line the pane draws.
    pub top: usize,
    /// How many **display columns** every drawn line is shifted left by.
    ///
    /// Columns and not chars, which is the stage-a bug this stage could have
    /// inherited: with a CJK line the two numbers differ, and a shift measured
    /// in chars puts every line at a different horizontal position from every
    /// other one.
    pub hcols: usize,
}

/// The document geometry for a page drawn into `area` — the whole page rect,
/// as the shell knows it. `None` when there is no document on screen: no file,
/// a refusal note, HISTORY, or no room.
pub fn doc_geom(area: Rect, v: &CodeView) -> Option<DocGeom> {
    if v.mode != Mode::File {
        return None;
    }
    geom_for(content_rect(split(area).body, v), v.open.as_ref()?)
}

/// The same geometry from the content rect the paint already has, so the paint
/// and the hit test run one implementation rather than two that agree today.
fn geom_for(content: Rect, open: &OpenFile) -> Option<DocGeom> {
    if open.note.is_some() || content.width == 0 || content.height == 0 {
        return None;
    }
    let total = open.lines.len();
    let gw = digits(total.max(1)).max(2);
    let gutter = gw as u16 + 1;
    if content.width <= gutter {
        return None;
    }
    let text_w = (content.width - gutter) as usize;
    let top = open
        .scroll
        .min(total.saturating_sub(content.height as usize));
    // The one horizontal shift the whole pane gets, computed so the cursor
    // cell is inside it. `cols` of the prefix, not its char count: this is the
    // conversion the buffer never does and the paint always must.
    let chars = open.chars(open.line);
    let before: String = chars.iter().take(open.col).collect();
    let cx = cols(&before);
    let cw = chars
        .get(open.col)
        .map_or(1, |c| cols(&c.to_string()).max(1));
    let hcols = (cx + cw).saturating_sub(text_w);
    Some(DocGeom {
        area: content,
        gutter,
        top,
        hcols,
    })
}

/// The `(line, char)` a screen cell points at, clamped into the document.
/// `None` when the cell is outside the text area, which is what keeps a press
/// on the tree or the header out of the document.
pub fn cell_to_pos(v: &CodeView, area: Rect, col: u16, row: u16) -> Option<(usize, usize)> {
    let g = doc_geom(area, v)?;
    let open = v.open.as_ref()?;
    if open.lines.is_empty() {
        return None;
    }
    if col < g.area.x || col >= g.area.right() || row < g.area.y || row >= g.area.bottom() {
        return None;
    }
    let line = (g.top + (row - g.area.y) as usize).min(open.lines.len() - 1);
    let text_x = g.area.x + g.gutter;
    if col < text_x {
        return Some((line, 0));
    }
    let want = g.hcols + (col - text_x) as usize;
    // Walk the line in columns, exactly as the paint does, and stop at the
    // char whose cell the press landed in. A wide glyph owns both its columns.
    let mut at = 0usize;
    for (i, ch) in open.chars(line).into_iter().enumerate() {
        let w = cols(&ch.to_string()).max(1);
        if want < at + w {
            return Some((line, i));
        }
        at += w;
    }
    Some((line, open.line_len(line)))
}

/// A click on the page, or nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodeClick {
    /// Switch to this tab.
    Tab(Mode),
    /// Hand the repository to the external editor.
    Editor,
    /// Copy the selection, or the whole file when there is none.
    Copy,
    /// Write the buffer back.
    Save,
    /// A cell in the document: this `(line, char)`.
    Doc(usize, usize),
    /// Anywhere in the chat strip. It takes the keyboard; the click does not
    /// carry a column, because a one-line box a person has just pointed at
    /// wants the cursor at the end of what is already typed.
    Strip,
}

/// Hit-test a press against the page's controls, using the same geometry the
/// paint used.
///
/// `None` for every other cell, deliberately: a page must not swallow the rest
/// of the surface.
pub fn click(v: &CodeView, area: Rect, col: u16, row: u16) -> Option<CodeClick> {
    let r = split(area);
    let inner = Block::bordered().inner(r.body);
    let bar = header_bar(inner, savable(v));
    let on = |rect: Option<Rect>| rect.is_some_and(|q| row == q.y && col >= q.x && col < q.right());
    if on(bar.save) {
        return Some(CodeClick::Save);
    }
    if on(bar.copy) {
        return Some(CodeClick::Copy);
    }
    if on(bar.editor) {
        return Some(CodeClick::Editor);
    }
    if on(bar.file_tab) {
        return Some(CodeClick::Tab(Mode::File));
    }
    if on(bar.history_tab) {
        return Some(CodeClick::Tab(Mode::History));
    }
    if let Some((l, c)) = cell_to_pos(v, area, col, row) {
        return Some(CodeClick::Doc(l, c));
    }
    // The strip's rows, which `content_rect` has already taken off the
    // document — the two rects are disjoint by construction, so this order is
    // defensive rather than load-bearing. Swapping the two branches leaves
    // every test green; what is *not* green is a `content_rect` that stops
    // reserving the rows, which is where the guarantee actually lives.
    if let Some(s) = strip_rect(r.body) {
        if row >= s.y && row < s.bottom() && col >= s.x && col < s.right() {
            return Some(CodeClick::Strip);
        }
    }
    None
}

/// What a [`CodeClick`] asks the shell to do, so the click and the key it
/// duplicates cannot drift apart: one dispatch, two ways in.
pub fn act(v: &mut CodeView, hit: CodeClick) -> CodeAction {
    match hit {
        CodeClick::Editor => {
            v.notice = None;
            CodeAction::LaunchEditor
        }
        CodeClick::Save => v.request_save(),
        CodeClick::Copy => {
            v.notice = None;
            v.copy()
        }
        CodeClick::Doc(line, c) => {
            v.press_doc(line, c);
            CodeAction::FocusChanged
        }
        CodeClick::Strip => {
            v.notice = None;
            v.focus = Focus::Chat;
            v.strip.cursor = v.strip.input.chars().count();
            CodeAction::FocusChanged
        }
        CodeClick::Tab(Mode::File) if v.mode == Mode::History => {
            v.notice = None;
            v.toggle_mode()
        }
        CodeClick::Tab(Mode::History) if v.mode == Mode::File => {
            v.notice = None;
            v.toggle_mode()
        }
        CodeClick::Tab(_) => {
            v.notice = None;
            CodeAction::FocusChanged
        }
    }
}

// endregion: Layout

// region: Render
// ---------------------------------------------------------------------------
// Render — pure; the shell owns whether the page is open
// ---------------------------------------------------------------------------

/// Draw the tree and the body, and report where the controls landed.
pub fn render(area: Rect, buf: &mut Buffer, v: &CodeView, skin: &Skin) -> Regions {
    let split = split(area);
    draw_tree(split.tree, buf, v, skin);
    let mut r = draw_body(split.body, buf, v, skin);
    r.tree = split.tree;
    r.body = split.body;
    r
}

fn draw_tree(area: Rect, buf: &mut Buffer, v: &CodeView, skin: &Skin) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let border = border_style(v.focus == Focus::Tree, skin);
    let block = Block::bordered()
        .border_style(border)
        .title(Span::styled(" EXPLORER ", skin.palette.bold(Role::Accent)));
    let inner = block.inner(area);
    block.render(area, buf);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let vis = v.visible();
    if vis.is_empty() {
        put(buf, inner, 0, Line::from(dim(skin, "no files here")));
        return;
    }

    let ascii = skin.glyphs == super::render::ASCII;
    let (open_g, shut_g) = if ascii { ("v", ">") } else { ("▾", "▸") };
    let h = inner.height as usize;
    let top = scroll_to_show(v.tree_scroll, v.tree_sel, h);
    let w = inner.width as usize;
    for (row, &node_i) in vis.iter().enumerate().skip(top).take(h) {
        let n = &v.nodes[node_i];
        let marker = if n.is_dir {
            if v.expanded.contains(&n.path) {
                open_g
            } else {
                shut_g
            }
        } else {
            " "
        };
        // A path is bytes somebody else chose; it reaches a cell through the
        // same sanitiser the file's lines do.
        let text = format!("{}{marker} {}", "  ".repeat(n.depth), sanitise(&n.name));
        let body = pad(&fit(&text, w, skin.glyphs.ellipsis), w);
        let style = if row == v.tree_sel {
            skin.palette.chip(Role::Accent)
        } else if n.is_dir {
            skin.palette.style(Role::Info)
        } else {
            skin.palette.style(Role::Text)
        };
        put(
            buf,
            inner,
            (row - top) as u16,
            Line::from(Span::styled(body, style)),
        );
    }
}

/// Draw the header, the notice row, the content and the help row, and report
/// the geometry a click will be tested against.
fn draw_body(area: Rect, buf: &mut Buffer, v: &CodeView, skin: &Skin) -> Regions {
    let mut out = Regions {
        body: area,
        ..Regions::default()
    };
    if area.width == 0 || area.height == 0 {
        return out;
    }
    let block = Block::bordered().border_style(border_style(v.focus != Focus::Tree, skin));
    let inner = block.inner(area);
    block.render(area, buf);
    if inner.width == 0 || inner.height == 0 {
        return out;
    }

    let bar = header_bar(inner, savable(v));
    out.file_tab = bar.file_tab;
    out.history_tab = bar.history_tab;
    out.editor = bar.editor;
    out.copy = bar.copy;
    out.save = bar.save;
    let (file_style, hist_style) = match v.mode {
        Mode::File => (skin.palette.chip(Role::Accent), skin.palette.dim()),
        Mode::History => (skin.palette.dim(), skin.palette.chip(Role::Accent)),
    };
    if let Some(rect) = bar.file_tab {
        Line::from(Span::styled(FILE_LABEL, file_style)).render(rect, buf);
    }
    if let (Some(f), Some(h)) = (bar.file_tab, bar.history_tab) {
        Line::from(Span::styled(" · ", skin.palette.dim()))
            .render(Rect::new(f.right(), inner.y, h.x - f.right(), 1), buf);
        Line::from(Span::styled(HISTORY_LABEL, hist_style)).render(h, buf);
    }
    if let Some(rect) = bar.editor {
        Line::from(Span::styled(EDITOR_LABEL, skin.palette.dim())).render(rect, buf);
    }
    if let Some(rect) = bar.copy {
        Line::from(Span::styled(COPY_LABEL, skin.palette.dim())).render(rect, buf);
    }
    if let Some(rect) = bar.save {
        // The one control on this row that is lit rather than dim, and only
        // when pressing it would do something: a `[Save]` that looks the same
        // whether or not there is anything to save is a button that has to be
        // pressed to be understood.
        let style = if v.dirty() {
            skin.palette.bold(Role::Warn)
        } else {
            skin.palette.dim()
        };
        Line::from(Span::styled(SAVE_LABEL, style)).render(rect, buf);
    }

    // Header left: what the body is showing. In the patch stage that is the
    // commit, not the file, because the commit is what a reader has to be able
    // to name to trust the patch under it.
    let showing_diff =
        v.mode == Mode::History && v.history.as_ref().is_some_and(|h| h.showing_diff);
    let title = if showing_diff {
        let h = v.history.as_ref().expect("checked just above");
        match h.commits.get(h.sel) {
            Some(c) => format!("{}  {}", c.short, sanitise(&c.subject)),
            None => "no commit selected".to_string(),
        }
    } else {
        match v.open.as_ref() {
            // The lock is the first thing on the row, before the path, because
            // it is the thing a person needs before they start typing.
            Some(o) if o.locked.is_some() => {
                format!(
                    "{}  ({})",
                    sanitise(&o.path),
                    o.locked.expect("checked just above")
                )
            }
            // A dot for a buffer that is not on disk, and the word EDIT when
            // letters are going into it. Two different facts — a person can be
            // editing with nothing changed yet, or have changes and have left
            // the editor — so they are two different marks.
            Some(o) => {
                let dot = if o.dirty { "● " } else { "" };
                let mode = if v.editing() { "  EDIT" } else { "" };
                format!("{dot}{}{mode}", sanitise(&o.path))
            }
            None => "no file selected".to_string(),
        }
    };
    let leftmost = bar
        .save
        .or(bar.copy)
        .or(bar.editor)
        .or(bar.file_tab)
        .map_or(inner.right(), |r| r.x);
    let title_w = leftmost.saturating_sub(inner.x + 1) as usize;
    put(
        buf,
        inner,
        0,
        Line::from(Span::styled(
            fit(&title, title_w, skin.glyphs.ellipsis),
            skin.palette.style(Role::Info),
        )),
    );

    // The notice row exists only when there is a notice, so a clean page
    // spends every remaining row on the file.
    let mut y = 1u16;
    if let Some(note) = &v.notice {
        put(
            buf,
            inner,
            y,
            Line::from(Span::styled(
                fit(&sanitise(note), inner.width as usize, skin.glyphs.ellipsis),
                skin.palette.style(Role::Warn),
            )),
        );
        y += 1;
    }

    // The same function the hit test calls, so a press cannot land on a row
    // the paint put somewhere else.
    let content = content_rect(area, v);
    let lsp_h = lsp_rows(v);
    // The same argument `content_rect` makes: the help row's position is
    // derived from the same three heights the content rect was, so a row can
    // never be drawn where the document already is. This used to pass `y`
    // alone, which agreed with `content_rect` only because the strip's floor
    // and the help row's happen to make the two answers equal; the language
    // server's row breaks that coincidence.
    let help_h = help_rows(inner.height, y + strip_rows(inner.height) + lsp_h);
    let content_h = content.height;
    if content.height > 0 {
        match v.mode {
            Mode::File => draw_file(content, buf, v, skin),
            Mode::History => draw_history(content, buf, v, skin),
        }
        out.rows = content.height as usize;
    }
    // The language server's one row, under the document and above the help.
    // Coloured by the status rather than by the worst diagnostic: the row is
    // mostly about whether there is a server at all, and a red row on a file
    // with one warning in it is the page shouting.
    if let Some(line) = lsp_line(v) {
        put(
            buf,
            inner,
            y + content_h,
            Line::from(Span::styled(
                fit(&sanitise(&line), inner.width as usize, skin.glyphs.ellipsis),
                skin.palette.style(v.lsp.status.role()),
            )),
        );
    }
    if help_h == 1 {
        put(
            buf,
            inner,
            y + content_h + lsp_h,
            Line::from(Span::styled(
                // The exit hint rides the help row rather than each branch of
                // `help_line`, so a mode added later cannot forget it, and it
                // goes first because this row is fitted to the pane: a hint
                // that says how to leave is the one part of it a reader who is
                // stuck needs, and the tail is what an ellipsis eats.
                fit(
                    &format!("{EXIT_HINT} · {}", help_line(v)),
                    inner.width as usize,
                    skin.glyphs.ellipsis,
                ),
                skin.palette.dim(),
            )),
        );
    }
    // The same function the hit test calls, for the same reason `content_rect`
    // is shared: a strip the paint puts somewhere the click does not look for
    // is a box that swallows presses.
    if let Some(strip) = strip_rect(area) {
        out.strip = Some(strip);
        draw_strip(strip, buf, v, skin);
    }
    // Last, and over the document rather than over the whole body: a popup
    // that covered the header would hide the path of the file it is about.
    if let Some(places) = v.lsp.places.as_ref() {
        // Above both, because it is the only one of the three that took the
        // keyboard: drawing a second box over a list somebody is arrowing
        // through would leave two things claiming the same arrow key.
        if content.width > 0 && content.height > 0 {
            draw_places(content, buf, places, skin);
        }
    } else if let Some(popup) = v.lsp.hover.as_ref() {
        if content.width > 0 && content.height > 0 {
            let rect = draw_hover(content, buf, popup, skin);
            out.hover = (rect.width > 0 && rect.height > 0).then_some(rect);
        }
    } else if let Some(popup) = v.lsp.popup.as_ref() {
        // Only when no hover is up. Two overlapping boxes over three lines of
        // code is a page nobody can read, and the hover was asked for
        // explicitly while this one can open on its own.
        if content.width > 0 && content.height > 0 {
            draw_popup(content, buf, v, popup, skin);
        }
    }
    out
}

/// The chat strip: a rule, the pointer to the last question, and the input.
///
/// The cursor is drawn as a styled cell rather than parked with the terminal's
/// own cursor, which is what the rest of this page does and what the frame
/// expects — `app.rs` returns no cursor position for the Code page. It is
/// listed as unverified in the report: a drawn cursor is a cell buffer's
/// answer, and a cell buffer is not a console.
fn draw_strip(area: Rect, buf: &mut Buffer, v: &CodeView, skin: &Skin) {
    if area.height < 3 || area.width == 0 {
        return;
    }
    put(
        buf,
        area,
        0,
        Line::from(Span::styled(
            skin.glyphs.rule.repeat(area.width as usize),
            skin.palette.dim(),
        )),
    );
    // What the row says depends on what there is to say, and every branch is a
    // true sentence about the current state rather than a label: a question
    // that went out and where its answer is, the file a question would be
    // about, or the reason there is nothing to ask.
    let pointer = match (&v.strip.sent, v.open.as_ref()) {
        (Some(q), _) => format!("asked: {q}   (the answer is in the transcript — Esc to read it)"),
        (None, Some(o)) if o.note.is_some() => {
            format!("{} cannot be read, so it cannot be asked about", o.path)
        }
        (None, Some(o)) => format!("ask emma about {}", o.path),
        (None, None) => "open a file to ask emma about it".to_string(),
    };
    put(
        buf,
        area,
        1,
        Line::from(Span::styled(
            fit(
                &sanitise(&pointer),
                area.width as usize,
                skin.glyphs.ellipsis,
            ),
            skin.palette.dim(),
        )),
    );

    let focused = v.focus == Focus::Chat;
    let prompt_style = if focused {
        skin.palette.bold(Role::Accent)
    } else {
        skin.palette.dim()
    };
    let text_w = (area.width as usize).saturating_sub(2);
    let shown = fit(&v.strip.input, text_w, skin.glyphs.ellipsis);
    let mut spans = vec![Span::styled("> ", prompt_style)];
    if focused {
        // Three spans so the cursor cell is the one the buffer's index names.
        // `fit` may have ellipsised, so the cursor is clamped to what is drawn
        // rather than pointing past the end of the row.
        let at = v.strip.cursor.min(shown.chars().count());
        let before: String = shown.chars().take(at).collect();
        let under: String = shown.chars().skip(at).take(1).collect();
        let after: String = shown.chars().skip(at + 1).collect();
        let under = if under.is_empty() {
            " ".to_string()
        } else {
            under
        };
        spans.push(Span::styled(before, skin.palette.style(Role::Text)));
        spans.push(Span::styled(under, skin.palette.chip(Role::Accent)));
        spans.push(Span::styled(after, skin.palette.style(Role::Text)));
    } else {
        spans.push(Span::styled(shown, skin.palette.style(Role::Text)));
    }
    put(buf, area, 2, Line::from(spans));
}

/// The key that leaves this page, in the words the page itself uses.
///
/// **Every other full-screen page has had one and this page had none**, which
/// the cross-page guarantee could not see, because the page was not in the list
/// that guarantee sweeps. Named here rather than in the test for the reason the
/// other three are: a constant the test owned would go on agreeing with itself
/// after the page changed its key.
///
/// `Alt+c` rather than `Esc`, because `Esc` is layered here: it leaves the
/// editor, then the ask box, then the page, so a hint promising the last of
/// those would be wrong twice before it was right.
pub const EXIT_HINT: &str = "Alt+c closes";

/// The page's keys, named where a reader will look for them. Four rows and not
/// one, because the editor's keys and the browser's mean different things on
/// the same keyboard and a row naming both would be a row naming neither.
fn help_line(v: &CodeView) -> String {
    // First, because the panel has the keyboard: a row naming the editor's
    // keys while the panel owns them would name keys that do nothing.
    if v.lsp.places.is_some() {
        return "↑/↓ pick a place · Enter opens it · any other key closes".to_string();
    }
    if v.focus == Focus::Chat {
        return "type a question about the open file · Enter sends it · Esc back to the file \
                · F2 saves"
            .to_string();
    }
    if v.editing() {
        return "typing edits · Ctrl+space or F9 completes · Ctrl+s or F2 saves \
                · Shift+arrows select · F4 copies · Esc read-only"
            .to_string();
    }
    match v.mode {
        Mode::History if v.history.as_ref().is_some_and(|h| h.showing_diff) => {
            "↑/↓ scroll the patch · b or Esc back to the commits · F3 or click FILE".to_string()
        }
        Mode::History => {
            "↑/↓ pick a commit · Enter shows its patch · b back to the file · F3 or click FILE"
                .to_string()
        }
        // One `concat!` rather than a `\`-continued literal. rustfmt joins a
        // continued string back onto one line and keeps the *leading* spaces of
        // the following line, which put twenty-four of them into the middle of
        // this row on the screen. `no_help_row_has_a_gap_in_it` is the receipt.
        Mode::File => concat!(
            "Tab pane, then the ask box · ↑/↓ move · Enter opens, then edits",
            " · F3 HISTORY · F5 hover · F6 definition · F8 uses",
            " · F10 outline · F9 complete · F7 editor",
        )
        .to_string(),
    }
}

/// The file, with the cursor and the selection over it.
///
/// The body is drawn a **cell at a time** rather than a line at a time, which
/// is the price of having a cursor: a selection band and a cursor cell are
/// per-character, and `fit` can only budget a whole string. The budget is kept
/// the harder way instead — `cols` of each glyph, and the loop stops before a
/// glyph that would not fit whole. A wide glyph at the right edge is dropped,
/// never half-drawn, which is the column-budget class this repository has
/// already paid for once.
fn draw_file(area: Rect, buf: &mut Buffer, v: &CodeView, skin: &Skin) {
    let Some(open) = v.open.as_ref() else {
        put(
            buf,
            area,
            0,
            // The exit hint belongs here too: with no file open this line is
            // the only thing on the pane, so a reader who cannot find the way
            // out has nowhere else to look.
            Line::from(dim(
                skin,
                &format!("{EXIT_HINT} · select a file: ↑/↓ move, Enter opens, F3 for history"),
            )),
        );
        return;
    };
    if let Some(note) = &open.note {
        put(
            buf,
            area,
            0,
            Line::from(Span::styled(sanitise(note), skin.palette.style(Role::Warn))),
        );
        return;
    }
    // The geometry the hit test reads, from the one function that computes it.
    // `None` means there is no room for a document at all.
    let Some(g) = geom_for(area, open) else {
        return;
    };
    let h = area.height as usize;
    let top = g.top;
    let gw = (g.gutter - 1) as usize;
    let text_w = (area.width - g.gutter) as usize;
    let hcols = g.hcols;
    let editing = v.editing();
    let sel = open.selection();
    // Diagnostics decorate this file only when they are *for* this file. A
    // stale set is not drawn dim; it is not drawn.
    let diags = (v.lsp.path.as_deref() == Some(open.path.as_str())).then_some(&v.lsp);
    // The server's own account of what each run of characters is. Empty when
    // there is no server, or when the answer is about a different file, which
    // draws plain text rather than colour from a stale set.
    let tokens: &[emma_tools_lsp::render::Token] =
        if v.lsp.tokens_for.as_deref() == Some(open.path.as_str()) {
            &v.lsp.tokens
        } else {
            &[]
        };
    for (i, ln) in open.lines.iter().enumerate().skip(top).take(h) {
        // **The line's own tokens, found once for the line rather than scanned
        // for out of the whole file once per character.** `syntax_role`'s doc
        // claimed the former and the code did the latter: this file yields
        // 28,330 tokens from rust-analyzer, and the inner loop asked all of
        // them about every cell it painted. The answer is ordered by line, so
        // the slice is a binary search.
        let line_tokens = tokens_on_line(tokens, i);
        let row_chars: Vec<char> = ln.chars().collect();
        let (sel_a, sel_b) = match sel {
            Some((a, b)) if i >= a.0 && i <= b.0 => (
                if i == a.0 { a.1 } else { 0 },
                if i == b.0 { b.1 } else { row_chars.len() },
            ),
            _ => (0, 0),
        };
        let cursor = (editing && i == open.line).then_some(open.col);
        // The worst diagnostic on this line takes the gutter's **separator**
        // column - the space between the number and the text - rather than a
        // column of its own. That is what keeps `geom_for`'s gutter width, and
        // therefore every hit test, from moving when a server answers: a page
        // whose columns shift under the pointer as diagnostics arrive is a
        // page where a click lands on a different character from the one it
        // was aimed at. It is also why the mark must stay one column wide.
        let worst = diags.and_then(|l| l.at_line(i));
        let (sep, sep_style) = match worst {
            Some(d) => (
                d.severity.mark().to_string(),
                skin.palette.bold(d.severity.role()),
            ),
            None => (" ".to_string(), skin.palette.dim()),
        };
        let mut spans = vec![
            Span::styled(format!("{:>gw$}", i + 1, gw = gw), skin.palette.dim()),
            Span::styled(sep, sep_style),
        ];
        // Walk the line in display columns, skipping what the horizontal shift
        // hides. A glyph straddling the left edge is replaced by a space: half
        // a wide character is a cell the terminal and ratatui disagree about.
        let mut at = 0usize; // columns consumed from the start of the line
        let mut x = 0usize; // columns painted into the pane
        for (idx, ch) in row_chars.iter().enumerate() {
            let w = cols(&ch.to_string()).max(1);
            if at + w <= hcols {
                at += w;
                continue;
            }
            let glyph = if at < hcols {
                " ".repeat(at + w - hcols)
            } else {
                ch.to_string()
            };
            let gwid = cols(&glyph).max(1);
            if x + gwid > text_w {
                break;
            }
            let selected = sel_a < sel_b && idx >= sel_a && idx < sel_b;
            // The squiggle a terminal cannot draw: bold and underlined in the
            // severity's colour, over exactly the span the server named. Under
            // the cursor and the selection, because those say where the person
            // is and this says where the compiler is.
            let marked = worst.filter(|d| covers(d, i, idx));
            let style = if cursor == Some(idx) {
                skin.palette.chip(Role::Accent)
            } else if selected {
                skin.palette.chip(Role::Info)
            } else if let Some(d) = marked {
                skin.palette
                    .bold(d.severity.role())
                    .add_modifier(ratatui::style::Modifier::UNDERLINED)
            } else {
                // **The lowest rung of the ladder**, and the order above it is
                // the point: the cursor says where the person is, the
                // selection what they have taken, the diagnostic where the
                // compiler objects, and syntax colour is what the text *is*.
                // A comment under the cursor is drawn as the cursor, because
                // losing the cursor is worse than losing the colour.
                skin.palette.style(syntax_role(line_tokens, idx))
            };
            spans.push(Span::styled(glyph, style));
            x += gwid;
            at += w;
        }
        // The cursor past the end of the line has no character to sit on, so
        // it gets a cell of its own — otherwise it vanishes at every line end.
        if cursor == Some(row_chars.len()) && x < text_w {
            spans.push(Span::styled(" ", skin.palette.chip(Role::Accent)));
        }
        put(buf, area, (i - top) as u16, Line::from(spans));
    }
}

/// Whether a diagnostic's span covers this `(line, char)`.
///
/// A zero-width span - which servers really do emit, for "something was
/// expected here" - still marks one cell, because a decoration nobody can see
/// is the same as no decoration.
fn covers(d: &Diag, line: usize, col: usize) -> bool {
    if !d.spans_line(line) {
        return false;
    }
    let start = if line == d.line { d.start_col } else { 0 };
    let end = if line == d.end_line {
        d.end_col.max(d.start_col + 1)
    } else {
        usize::MAX
    };
    col >= start && col < end
}

/// Clear a rectangle, draw the border round it, and hand back the inside.
///
/// **The clearing is the part worth a function.** Every floating thing on this
/// page — the hover, the completion list, the places panel — is drawn last, on
/// top of a document that has already been painted, and a border laid over live
/// text with the text still showing through reads as corruption rather than as
/// an overlay. The rule was written out three times and the argument for it
/// once, so two of the three could have lost it without anybody noticing.
///
/// The order matters and is the reason this is not two calls: the fill covers
/// only the inside, so it must happen before `render` puts the border on the
/// cells around it — filling afterwards would erase the border's own row.
fn overlay(buf: &mut Buffer, rect: Rect, skin: &Skin) -> Rect {
    let block = Block::bordered().border_style(skin.palette.style(Role::Accent));
    let inner = block.inner(rect);
    for y in inner.y..inner.bottom() {
        put(
            buf,
            Rect::new(inner.x, y, inner.width, 1),
            0,
            Line::from(Span::styled(
                " ".repeat(inner.width as usize),
                skin.palette.style(Role::Text),
            )),
        );
    }
    block.render(rect, buf);
    inner
}

/// The hover popup, over the document.
///
/// Bounded on both axes and drawn last, so it covers the text rather than
/// being covered by it. The rect is returned for the same reason the header's
/// buttons are: a thing on screen whose position nothing reported cannot be
/// tested. A zero-sized rect means there was no room and nothing was drawn -
/// the popup is not squeezed into a pane that cannot hold it, because a border
/// with one row of text inside it says less than the file it would cover.
fn draw_hover(area: Rect, buf: &mut Buffer, popup: &HoverPopup, skin: &Skin) -> Rect {
    let want_h = (popup.lines.len().min(HOVER_MAX_LINES) as u16 + 2).min(area.height);
    let want_w = popup
        .lines
        .iter()
        .map(|l| cols(l))
        .max()
        .unwrap_or(20)
        .clamp(20, area.width.saturating_sub(2).max(20) as usize) as u16
        + 2;
    if want_h < 3 || area.width < 6 {
        return Rect::new(area.x, area.y, 0, 0);
    }
    let rect = Rect::new(area.x, area.y, want_w.min(area.width), want_h);
    let inner = overlay(buf, rect, skin);
    for (row, line) in popup
        .lines
        .iter()
        .skip(popup.scroll)
        .take(inner.height as usize)
        .enumerate()
    {
        put(
            buf,
            inner,
            row as u16,
            Line::from(Span::styled(
                // The server's own bytes, so the one sanitiser this codebase
                // has stands between them and a cell: rust-analyzer quotes
                // source into a hover, and source is where the control bytes
                // this page already refuses to draw come from.
                fit(&sanitise(line), inner.width as usize, skin.glyphs.ellipsis),
                skin.palette.style(Role::Text),
            )),
        );
    }
    rect
}

/// The completion popup, under the cursor's line where possible.
///
/// **Under the line rather than over it**, which is not a style choice: the
/// line being typed is the one thing that must stay visible, and a box drawn on
/// top of it hides the word the list is about. When there is no room below, it
/// goes above for the same reason.
///
/// Every colour is a `Role`, and the only glyphs are the skin's own, so the
/// ASCII skin and a terminal with no colour both draw something legible. The
/// chosen row is marked by a styled cell rather than by colour alone, because
/// colour alone is not a marker on a monochrome terminal.
fn draw_popup(area: Rect, buf: &mut Buffer, v: &CodeView, popup: &Popup, skin: &Skin) {
    let visible = popup.visible();
    if visible.is_empty() || area.width < 12 {
        return;
    }
    let rows = visible.len().min(POPUP_MAX_ROWS);
    let want_h = rows as u16 + 2;
    if want_h > area.height || area.height < 3 {
        return;
    }

    // The widest row, capped so the popup never takes the whole pane: a list
    // that covers the file is one that hides the answer it is about.
    let width = visible
        .iter()
        .take(rows)
        .map(|c| {
            cols(&c.label)
                + c.detail.as_deref().map(|d| cols(d) + 2).unwrap_or(0)
                + if c.kind.is_empty() {
                    0
                } else {
                    cols(c.kind) + 3
                }
        })
        .max()
        .unwrap_or(20)
        .clamp(12, (area.width.saturating_sub(2)).max(12) as usize) as u16
        + 2;

    // Below the cursor's row when it fits, above when it does not.
    let cursor_row = v
        .open
        .as_ref()
        .and_then(|o| doc_geom(area, v).map(|g| (o.line, g)))
        .map(|(line, g)| area.y + (line.saturating_sub(g.top)) as u16)
        .unwrap_or(area.y);
    let below = cursor_row.saturating_add(1);
    let y = if below + want_h <= area.bottom() {
        below
    } else {
        cursor_row.saturating_sub(want_h).max(area.y)
    };
    let x = area.x.min(area.right().saturating_sub(width));
    let rect = Rect::new(x, y, width.min(area.width), want_h);

    let inner = overlay(buf, rect, skin);

    // Keep the chosen row on screen when the list is longer than the box.
    let at = popup.at.min(visible.len().saturating_sub(1));
    let first = at.saturating_sub(rows.saturating_sub(1));
    for (row, item) in visible.iter().skip(first).take(rows).enumerate() {
        let chosen = first + row == at;
        let mut spans = vec![Span::styled(
            // The pointer is a marker, not a colour: on a monochrome terminal
            // colour alone says nothing about which row is chosen.
            if chosen {
                format!("{} ", skin.glyphs.bullet)
            } else {
                "  ".to_string()
            },
            skin.palette
                .style(if chosen { Role::Accent } else { Role::Dim }),
        )];
        spans.push(Span::styled(
            sanitise(&item.label),
            skin.palette
                .style(if chosen { Role::Accent } else { Role::Text }),
        ));
        if !item.kind.is_empty() {
            spans.push(Span::styled(
                format!("  {}", item.kind),
                skin.palette.style(Role::Dim),
            ));
        }
        if let Some(detail) = item.detail.as_deref() {
            spans.push(Span::styled(
                format!("  {}", sanitise(detail)),
                skin.palette.style(Role::Dim),
            ));
        }
        put(buf, inner, row as u16, Line::from(spans));
    }
}

/// The places panel: references to a symbol, or the symbols in a file.
///
/// **On the right half of the document when there is room for one**, which is
/// the one layout decision here worth an argument. The list is a set of
/// destinations rather than an annotation on a line, so unlike the hover and
/// the completion popup it has no line it must stay next to — and keeping the
/// left half clear means the code stays readable while somebody arrows through
/// the answers about it. On a narrow pane it takes the whole width, because
/// half of a narrow pane is a column of ellipses.
///
/// The chosen row is a styled cell rather than a colour, the rule every list
/// on this page follows: colour alone is not a marker on a monochrome
/// terminal.
fn draw_places(area: Rect, buf: &mut Buffer, places: &Places, skin: &Skin) {
    if places.rows.is_empty() || area.width < 12 || area.height < 4 {
        return;
    }
    let rows = places.rows.len().min(PLACES_MAX_ROWS).min(
        // Two for the border, one for the header.
        area.height.saturating_sub(3) as usize,
    );
    if rows == 0 {
        return;
    }
    let width = if area.width >= 60 {
        area.width / 2
    } else {
        area.width
    };
    let rect = Rect::new(
        area.right().saturating_sub(width),
        area.y,
        width,
        rows as u16 + 3,
    );

    let inner = overlay(buf, rect, skin);

    let w = inner.width as usize;
    // The header says what was asked and how many answers came back, because a
    // list of twelve rows out of forty is a different fact from a list of
    // twelve, and the panel is the only place that difference can be told.
    let head = format!("{} ({})", sanitise(&places.about), places.rows.len());
    put(
        buf,
        inner,
        0,
        Line::from(Span::styled(
            fit(&head, w, skin.glyphs.ellipsis),
            skin.palette.style(Role::Dim),
        )),
    );

    let at = places.at.min(places.rows.len() - 1);
    // No stored first row: with one selection and no wheel, "keep the chosen
    // row on screen" is the whole scrolling rule, and a stored offset would be
    // a second thing to keep in step with it.
    let first = scroll_to_show(0, at, rows);
    for (row, place) in places.rows.iter().enumerate().skip(first).take(rows) {
        let chosen = row == at;
        let text = fit(&sanitise(&place.label), w, skin.glyphs.ellipsis);
        let style = if chosen {
            skin.palette.chip(Role::Accent)
        } else {
            skin.palette.style(Role::Text)
        };
        put(
            buf,
            inner,
            (row - first) as u16 + 1,
            Line::from(Span::styled(pad(&text, w), style)),
        );
    }
}

fn draw_history(area: Rect, buf: &mut Buffer, v: &CodeView, skin: &Skin) {
    let Some(hist) = v.history.as_ref() else {
        put(buf, area, 0, Line::from(dim(skin, "loading history…")));
        return;
    };
    if hist.commits.is_empty() {
        let msg = hist
            .note
            .clone()
            .unwrap_or_else(|| "no history for this file".to_string());
        put(buf, area, 0, Line::from(dim(skin, &sanitise(&msg))));
        return;
    }
    if hist.showing_diff {
        draw_diff(area, buf, hist, skin);
        return;
    }

    // The commit list, over the whole region: one screen of history, not a
    // third of one. Enter opens the selected commit's patch.
    let h = area.height as usize;
    let w = area.width as usize;
    let top = scroll_to_show(hist.scroll, hist.sel, h);
    for (row, c) in hist.commits.iter().enumerate().skip(top).take(h) {
        let y = (row - top) as u16;
        let subject = sanitise(&c.subject);
        if row == hist.sel {
            let text = format!("{}  {}  {}", c.short, c.date, subject);
            let body = pad(&fit(&text, w, skin.glyphs.ellipsis), w);
            put(
                buf,
                area,
                y,
                Line::from(Span::styled(body, skin.palette.chip(Role::Accent))),
            );
        } else {
            let subj_w = w.saturating_sub(cols(&c.short) + cols(&c.date) + 4);
            let line = Line::from(vec![
                Span::styled(format!("{}  ", c.short), skin.palette.style(Role::Accent)),
                Span::styled(format!("{}  ", c.date), skin.palette.dim()),
                Span::styled(
                    fit(&subject, subj_w, skin.glyphs.ellipsis),
                    skin.palette.style(Role::Text),
                ),
            ]);
            put(buf, area, y, line);
        }
    }
}

/// The selected commit's patch for the open file, over the whole region.
fn draw_diff(area: Rect, buf: &mut Buffer, hist: &History, skin: &Skin) {
    let h = area.height as usize;
    if hist.diff.is_empty() {
        // Two different truths, and the page must not pick the wrong one: no
        // answer yet is not the same as an answer that was empty.
        let msg = match hist.diff_for {
            Some(_) => "this commit changed nothing in this file",
            None => "loading the patch…",
        };
        put(buf, area, 0, Line::from(dim(skin, msg)));
        return;
    }
    let top = hist.diff_scroll.min(hist.diff.len().saturating_sub(h));
    for (j, row) in hist.diff.iter().enumerate().skip(top).take(h) {
        let style = match row.kind {
            DiffKind::Add => skin.palette.style(Role::Ok),
            DiffKind::Del => skin.palette.style(Role::Err),
            DiffKind::Hunk => skin.palette.style(Role::Accent),
            DiffKind::Meta => skin.palette.dim(),
            DiffKind::Context => skin.palette.style(Role::Text),
        };
        // A patch is file content, so it carries whatever the file carried.
        put(
            buf,
            area,
            (j - top) as u16,
            Line::from(Span::styled(
                fit(
                    &sanitise(&row.text),
                    area.width as usize,
                    skin.glyphs.ellipsis,
                ),
                style,
            )),
        );
    }
}

// endregion: Render

// region: Small helpers
// ---------------------------------------------------------------------------

fn border_style(focused: bool, skin: &Skin) -> ratatui::style::Style {
    if focused {
        skin.palette.style(Role::Accent)
    } else {
        skin.palette.dim()
    }
}

fn dim<'a>(skin: &Skin, text: &str) -> Span<'a> {
    Span::styled(text.to_string(), skin.palette.dim())
}

/// Render one line at row `y` of `area`, clipped to the pane.
fn put(buf: &mut Buffer, area: Rect, y: u16, line: Line) {
    if y >= area.height {
        return;
    }
    line.render(Rect::new(area.x, area.y + y, area.width, 1), buf);
}

/// Pad `s` with spaces to `width` display columns, for a chip band. Never
/// truncates — the caller has already [`fit`]ted.
fn pad(s: &str, width: usize) -> String {
    let c = cols(s);
    if c >= width {
        s.to_string()
    } else {
        format!("{s}{}", " ".repeat(width - c))
    }
}

/// The top row a windowed list should start at so `sel` stays visible.
fn scroll_to_show(scroll: usize, sel: usize, height: usize) -> usize {
    if height == 0 {
        return 0;
    }
    if sel < scroll {
        sel
    } else if sel >= scroll + height {
        sel + 1 - height
    } else {
        scroll
    }
}

fn digits(mut n: usize) -> usize {
    let mut d = 1;
    while n >= 10 {
        n /= 10;
        d += 1;
    }
    d
}

// endregion: Small helpers

#[cfg(test)]
mod tests {
    use super::*;
    use crate::term::code_git::TextFile;
    use crate::term::palette::{Level, Palette};
    use crate::term::render::UNICODE;

    fn skin() -> Skin {
        Skin::new(Palette::new(Level::Truecolor), UNICODE)
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::from(code)
    }

    fn paths() -> Vec<String> {
        vec![
            "docs/index.html".to_string(),
            "src/main.rs".to_string(),
            "src/term/code.rs".to_string(),
            "README.md".to_string(),
        ]
    }

    fn sample() -> CodeView {
        CodeView::new(PathBuf::from("C:/src/emma"), build_nodes(&paths()))
    }

    fn text(lines: &[&str]) -> FileRead {
        FileRead::Text(TextFile {
            lines: lines.iter().map(|s| (*s).to_string()).collect(),
            ending: crate::term::code_git::LineEnding::Lf,
            trailing_newline: true,
        })
    }

    /// The hash the shell would have taken beside this read: the hash of the
    /// bytes the file actually is. A test that wants an *editable* buffer
    /// passes this; a test that wants a locked one passes something else, and
    /// that is the whole difference between the two.
    fn disk(read: &FileRead) -> Option<u64> {
        match read {
            FileRead::Text(t) => Some(hash_bytes(
                joined(&t.lines, t.ending, t.trailing_newline).as_bytes(),
            )),
            FileRead::Refused(_) => None,
        }
    }

    /// Open a file into the page the way the shell does, with the two answers
    /// agreeing, so the buffer is editable.
    fn open_file(v: &mut CodeView, path: &str, lines: &[&str]) {
        let read = text(lines);
        let hash = disk(&read);
        v.set_open(path.to_string(), read, hash);
    }

    /// Open a file and start editing it, which is two keys on the real page.
    fn editing_at(v: &mut CodeView, path: &str, lines: &[&str]) {
        open_file(v, path, lines);
        v.body_rows = 10;
        v.focus = Focus::Body;
        assert_eq!(handle_key(v, key(KeyCode::Enter)), CodeAction::FocusChanged);
        assert!(v.editing(), "the buffer should have become editable");
    }

    fn dump(buf: &Buffer) -> Vec<String> {
        (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect()
    }

    fn painted(v: &CodeView, area: Rect) -> (Buffer, Regions) {
        let mut buf = Buffer::empty(area);
        let r = render(area, &mut buf, v, &skin());
        (buf, r)
    }

    fn commit(short: &str, subject: &str) -> Commit {
        Commit {
            hash: format!("{short}0000000000000000000000000000000000"),
            short: short.to_string(),
            author: "A".to_string(),
            date: "2026-09-06".to_string(),
            subject: subject.to_string(),
        }
    }

    // -----------------------------------------------------------------------
    // The tree
    // -----------------------------------------------------------------------

    #[test]
    fn build_nodes_sorts_dirs_before_files_at_each_level() {
        let n = build_nodes(&paths());
        let names: Vec<&str> = n.iter().map(|x| x.name.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "docs",
                "index.html",
                "src",
                "term",
                "code.rs",
                "main.rs",
                "README.md"
            ]
        );
    }

    #[test]
    fn a_collapsed_tree_hides_the_subtree_and_expanding_shows_it() {
        let mut v = sample();
        assert_eq!(v.visible().len(), 3, "docs, src, README.md");
        v.expanded.insert("docs".to_string());
        assert_eq!(v.visible().len(), 4);
    }

    #[test]
    fn enter_on_a_dir_expands_and_on_a_file_asks_the_shell_to_open_it() {
        let mut v = sample();
        assert_eq!(
            handle_key(&mut v, key(KeyCode::Enter)),
            CodeAction::FocusChanged
        );
        assert!(v.expanded.contains("docs"));
        // Move to the file inside it and open that.
        handle_key(&mut v, key(KeyCode::Down));
        assert_eq!(
            handle_key(&mut v, key(KeyCode::Enter)),
            CodeAction::Open("docs/index.html".to_string())
        );
    }

    #[test]
    fn left_on_a_file_jumps_to_its_parent_directory() {
        let mut v = sample();
        v.expanded.insert("docs".to_string());
        v.tree_sel = 1; // docs/index.html
        handle_key(&mut v, key(KeyCode::Left));
        assert_eq!(v.selected_node().unwrap().path, "docs");
    }

    // -----------------------------------------------------------------------
    // What never reaches a cell
    // -----------------------------------------------------------------------

    #[test]
    fn a_files_control_bytes_are_replaced_before_the_buffer_holds_them() {
        let read = text(&["let x = 1;\u{1b}[31m", "\tindented"]);
        let hash = disk(&read);
        let o = OpenFile::from_read("src/main.rs".to_string(), read, hash);
        assert!(
            !o.lines.iter().any(|l| l.contains('\u{1b}')),
            "an ESC survived the read: {:?}",
            o.lines
        );
        assert_eq!(o.lines[1], "    indented", "a TAB is not one cell");
        assert_eq!(
            o.locked,
            Some(SANITISED_NOTE),
            "the page must know the bytes were changed, and lock the save on it"
        );
        assert!(
            !o.editable(),
            "a buffer that is not the file cannot be typed into"
        );
    }

    #[test]
    fn a_sanitised_file_says_so_in_the_header_so_a_save_can_refuse_it() {
        let mut v = sample();
        open_file(&mut v, "src/main.rs", &["\tx"]);
        let (buf, _) = painted(&v, Rect::new(0, 0, 100, 12));
        let head = dump(&buf).join("\n");
        assert!(head.contains(SANITISED_NOTE), "{head}");
    }

    #[test]
    fn a_clean_file_is_not_marked_as_changed() {
        let read = text(&["fn main() {}"]);
        let hash = disk(&read);
        let o = OpenFile::from_read("a.rs".to_string(), read, hash);
        assert_eq!(o.locked, None);
        assert!(o.editable());
    }

    #[test]
    fn a_commit_subject_and_a_patch_row_lose_their_escape_bytes_too() {
        let mut v = sample();
        open_file(&mut v, "src/main.rs", &["x"]);
        v.set_history(vec![commit("aaaaaaa", "subject \u{1b}[31mred")], None);
        v.mode = Mode::History;
        let (buf, _) = painted(&v, Rect::new(0, 0, 100, 12));
        assert!(!dump(&buf).join("").contains('\u{1b}'));

        v.set_diff(
            "aaaaaaa0000000000000000000000000000000000",
            vec![DiffRow {
                kind: DiffKind::Add,
                text: "+let x = \u{1b}[31m1;".to_string(),
            }],
        );
        v.history.as_mut().unwrap().showing_diff = true;
        let (buf, _) = painted(&v, Rect::new(0, 0, 100, 12));
        assert!(!dump(&buf).join("").contains('\u{1b}'));
    }

    // -----------------------------------------------------------------------
    // History
    // -----------------------------------------------------------------------

    #[test]
    fn f3_with_no_file_open_says_why_rather_than_doing_nothing() {
        let mut v = sample();
        assert_eq!(
            handle_key(&mut v, key(KeyCode::F(3))),
            CodeAction::FocusChanged
        );
        assert!(v.notice.as_deref().unwrap().contains("open a file first"));
        assert_eq!(v.mode, Mode::File);
    }

    #[test]
    fn f3_with_a_file_open_asks_the_shell_to_load_its_history() {
        let mut v = sample();
        open_file(&mut v, "src/main.rs", &["x"]);
        assert_eq!(
            handle_key(&mut v, key(KeyCode::F(3))),
            CodeAction::LoadHistory
        );
        assert_eq!(v.mode, Mode::History);
    }

    #[test]
    fn selecting_the_next_commit_asks_for_its_patch_and_drops_the_old_one() {
        let mut v = sample();
        open_file(&mut v, "src/main.rs", &["x"]);
        handle_key(&mut v, key(KeyCode::F(3)));
        let first = v
            .set_history(
                vec![commit("aaaaaaa", "one"), commit("bbbbbbb", "two")],
                None,
            )
            .unwrap();
        v.set_diff(
            &first,
            vec![DiffRow {
                kind: DiffKind::Add,
                text: "+a".into(),
            }],
        );
        match handle_key(&mut v, key(KeyCode::Down)) {
            CodeAction::LoadDiff(h) => assert!(h.starts_with("bbbbbbb")),
            other => panic!("{other:?}"),
        }
        assert!(
            v.history.as_ref().unwrap().diff.is_empty(),
            "the stale patch stayed"
        );
    }

    #[test]
    fn a_patch_for_a_commit_that_is_no_longer_selected_is_dropped() {
        let mut v = sample();
        open_file(&mut v, "src/main.rs", &["x"]);
        v.set_history(
            vec![commit("aaaaaaa", "one"), commit("bbbbbbb", "two")],
            None,
        );
        v.mode = Mode::History;
        v.focus = Focus::Commits;
        handle_key(&mut v, key(KeyCode::Down)); // now on bbbbbbb
        let taken = v.set_diff(
            "aaaaaaa0000000000000000000000000000000000",
            vec![DiffRow {
                kind: DiffKind::Add,
                text: "+stale".into(),
            }],
        );
        assert!(!taken, "a late answer for the old commit was taken");
        assert!(v.history.as_ref().unwrap().diff.is_empty());
    }

    #[test]
    fn enter_on_a_commit_shows_its_patch_and_names_the_commit_in_the_header() {
        let mut v = sample();
        open_file(&mut v, "src/main.rs", &["x"]);
        v.mode = Mode::History;
        v.focus = Focus::Commits;
        let first = v
            .set_history(vec![commit("aaaaaaa", "the subject")], None)
            .unwrap();
        match handle_key(&mut v, key(KeyCode::Enter)) {
            CodeAction::LoadDiff(h) => assert_eq!(h, first),
            other => panic!("{other:?}"),
        }
        v.set_diff(
            &first,
            vec![DiffRow {
                kind: DiffKind::Context,
                text: " ctx".into(),
            }],
        );
        let (buf, _) = painted(&v, Rect::new(0, 0, 100, 12));
        let rows = dump(&buf);
        assert!(rows.join("\n").contains("the subject"), "{rows:#?}");
        assert!(rows.join("\n").contains("ctx"));
    }

    #[test]
    fn b_walks_back_from_the_patch_to_the_list_and_then_to_the_file() {
        let mut v = sample();
        open_file(&mut v, "src/main.rs", &["x"]);
        v.mode = Mode::History;
        v.focus = Focus::Commits;
        v.set_history(vec![commit("aaaaaaa", "one")], None);
        handle_key(&mut v, key(KeyCode::Enter));
        assert!(v.history.as_ref().unwrap().showing_diff);
        handle_key(&mut v, key(KeyCode::Char('b')));
        assert!(!v.history.as_ref().unwrap().showing_diff);
        handle_key(&mut v, key(KeyCode::Char('b')));
        assert_eq!(v.mode, Mode::File);
    }

    #[test]
    fn a_file_with_no_history_says_so_rather_than_drawing_an_empty_pane() {
        let mut v = sample();
        open_file(&mut v, "src/main.rs", &["x"]);
        v.mode = Mode::History;
        assert_eq!(v.set_history(Vec::new(), None), None);
        let (buf, _) = painted(&v, Rect::new(0, 0, 100, 12));
        assert!(dump(&buf).join("\n").contains("no history for this file"));
    }

    #[test]
    fn an_unanswered_patch_says_loading_and_never_changed_nothing() {
        let mut v = sample();
        open_file(&mut v, "src/main.rs", &["x"]);
        v.mode = Mode::History;
        v.focus = Focus::Commits;
        v.set_history(vec![commit("aaaaaaa", "one")], None);
        handle_key(&mut v, key(KeyCode::Enter));
        let (buf, _) = painted(&v, Rect::new(0, 0, 100, 12));
        let s = dump(&buf).join("\n");
        assert!(s.contains("loading the patch"), "{s}");
        assert!(!s.contains("changed nothing"));
    }

    // -----------------------------------------------------------------------
    // The viewer
    // -----------------------------------------------------------------------

    #[test]
    fn the_body_shows_a_file_with_line_numbers() {
        let mut v = sample();
        open_file(&mut v, "src/main.rs", &["fn main() {", "}"]);
        let (buf, _) = painted(&v, Rect::new(0, 0, 100, 12));
        let s = dump(&buf).join("\n");
        assert!(s.contains(" 1 fn main() {"), "{s}");
        assert!(s.contains(" 2 }"), "{s}");
    }

    #[test]
    fn a_refused_read_shows_the_refusal_instead_of_content() {
        let mut v = sample();
        v.set_open(
            "a.bin".to_string(),
            FileRead::Refused("binary file".to_string()),
            None,
        );
        let (buf, _) = painted(&v, Rect::new(0, 0, 100, 12));
        assert!(dump(&buf).join("\n").contains("binary file"));
    }

    #[test]
    fn a_long_file_paints_every_row_of_the_pane_and_the_paint_reports_them() {
        let lines: Vec<String> = (0..200).map(|i| format!("line {i}")).collect();
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        let mut v = sample();
        open_file(&mut v, "big.rs", &refs);
        let (buf, r) = painted(&v, Rect::new(0, 0, 100, 24));
        // Was 18 before stage c; the chat strip takes three of the body's
        // rows on a pane this tall, which is the trade the strip's own doc
        // argues for. The invariant under it is unchanged: every row the
        // paint *reports* has a line of the file on it.
        assert!(r.rows >= 17, "the body reported {} rows", r.rows);
        let painted_rows = dump(&buf)
            .iter()
            .filter(|row| row.contains("line "))
            .count();
        assert_eq!(painted_rows, r.rows, "a reported row was left blank");
    }

    #[test]
    fn the_wheel_scrolls_the_document_and_not_the_tree() {
        let lines: Vec<String> = (0..200).map(|i| format!("line {i}")).collect();
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        let mut v = sample();
        open_file(&mut v, "big.rs", &refs);
        let before = v.tree_sel;
        assert_eq!(v.wheel(false), CodeAction::FocusChanged);
        assert_eq!(v.open.as_ref().unwrap().scroll, WHEEL_ROWS as usize);
        assert_eq!(v.tree_sel, before);
        v.wheel(true);
        assert_eq!(v.open.as_ref().unwrap().scroll, 0);
    }

    #[test]
    fn page_down_moves_by_the_screen_the_paint_reported() {
        let lines: Vec<String> = (0..200).map(|i| format!("line {i}")).collect();
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        let mut v = sample();
        open_file(&mut v, "big.rs", &refs);
        let (_, r) = painted(&v, Rect::new(0, 0, 100, 24));
        v.body_rows = r.rows;
        handle_key(&mut v, key(KeyCode::PageDown));
        assert_eq!(v.open.as_ref().unwrap().scroll, r.rows);
    }

    // -----------------------------------------------------------------------
    // The second door, and what the page refuses
    // -----------------------------------------------------------------------

    #[test]
    fn f7_asks_the_shell_to_launch_the_external_editor() {
        let mut v = sample();
        assert_eq!(
            handle_key(&mut v, key(KeyCode::F(7))),
            CodeAction::LaunchEditor
        );
    }

    #[test]
    fn the_editor_button_is_where_the_paint_drew_it_and_does_the_same_thing() {
        let v = sample();
        let area = Rect::new(0, 0, 100, 12);
        let (buf, r) = painted(&v, area);
        let rect = r.editor.expect("the pane is wide enough for the button");
        assert!(
            dump(&buf)[rect.y as usize].contains(EDITOR_LABEL),
            "the button is not where the layout said"
        );
        let hit = click(&v, area, rect.x, rect.y).expect("a press on the button");
        assert_eq!(hit, CodeClick::Editor);
        let mut v = v;
        assert_eq!(act(&mut v, hit), CodeAction::LaunchEditor);
    }

    #[test]
    fn clicking_the_history_tab_asks_for_that_tab_and_file_comes_back() {
        let mut v = sample();
        open_file(&mut v, "src/main.rs", &["x"]);
        let area = Rect::new(0, 0, 100, 12);
        let (_, r) = painted(&v, area);
        let tab = r.history_tab.unwrap();
        let hit = click(&v, area, tab.x, tab.y).unwrap();
        assert_eq!(hit, CodeClick::Tab(Mode::History));
        assert_eq!(act(&mut v, hit), CodeAction::LoadHistory);
        let file = r.file_tab.unwrap();
        let hit = click(&v, area, file.x, file.y).unwrap();
        assert_eq!(act(&mut v, hit), CodeAction::FocusChanged);
        assert_eq!(v.mode, Mode::File);
    }

    #[test]
    fn a_press_on_the_body_is_not_a_control_and_falls_through() {
        let v = sample();
        let area = Rect::new(0, 0, 100, 12);
        assert_eq!(click(&v, area, 50, 8), None);
    }

    #[test]
    fn esc_closes_the_page_and_alt_and_ctrl_chords_fall_through() {
        let mut v = sample();
        assert_eq!(handle_key(&mut v, key(KeyCode::Esc)), CodeAction::Close);
        // Every one of these is a key the browse layer *would* take bare, so
        // the assertion cannot pass by the chord happening to be unbound —
        // which is how the first draft of this test passed under the mutation
        // that deleted the refusal outright.
        let mut open = sample();
        open_file(&mut open, "src/main.rs", &["x"]);
        for m in [KeyModifiers::ALT, KeyModifiers::CONTROL] {
            for code in [
                KeyCode::Char('j'),
                KeyCode::Char('g'),
                KeyCode::Char('l'),
                KeyCode::Down,
                KeyCode::Enter,
                KeyCode::Esc,
                KeyCode::Tab,
            ] {
                let mut v = open.clone();
                assert_eq!(
                    handle_key(&mut v, KeyEvent::new(code, m)),
                    CodeAction::None,
                    "{m:?}+{code:?} was swallowed by the page"
                );
            }
        }
    }

    /// The shell asks [`takes_key`] whether to hand the key on; the page asks
    /// it whether to act. One answer, so a key cannot be both refused by the
    /// page and swallowed by the reader.
    #[test]
    fn the_predicate_the_shell_reads_agrees_with_what_the_page_does() {
        let mut v = sample();
        for m in [KeyModifiers::NONE, KeyModifiers::ALT, KeyModifiers::CONTROL] {
            let k = KeyEvent::new(KeyCode::Char('j'), m);
            let before = takes_key(&v, k);
            let acted = handle_key(&mut v, k) != CodeAction::None;
            assert_eq!(before, acted, "{m:?} disagrees");
        }
    }

    #[test]
    fn a_key_release_is_never_the_pages() {
        let mut v = sample();
        let mut k = key(KeyCode::Enter);
        k.kind = KeyEventKind::Release;
        assert_eq!(handle_key(&mut v, k), CodeAction::None);
    }

    #[test]
    fn opening_a_file_drops_the_previous_files_history() {
        let mut v = sample();
        open_file(&mut v, "a.rs", &["x"]);
        v.set_history(vec![commit("aaaaaaa", "one")], None);
        open_file(&mut v, "b.rs", &["y"]);
        assert!(
            v.history.is_none(),
            "the old file's commits survived into the new file's page"
        );
    }

    #[test]
    fn a_narrow_pane_draws_no_controls_rather_than_controls_off_the_edge() {
        let v = sample();
        let area = Rect::new(0, 0, 30, 8);
        let (_, r) = painted(&v, area);
        // Both edges. Checking only the right one let a mutation that dropped
        // the button's width guard draw it *left* of the pane and still pass.
        let inner = Block::bordered().inner(r.body);
        for rect in [r.file_tab, r.history_tab, r.editor].into_iter().flatten() {
            assert!(
                rect.x >= inner.x && rect.right() <= inner.right(),
                "{rect:?} escapes {inner:?}"
            );
        }
    }

    // -----------------------------------------------------------------------
    // Stage b: the editor, the save path, the clipboard, and the column budget
    // -----------------------------------------------------------------------

    use crate::term::code_git::save_file;
    use std::path::Path;
    use tempfile::TempDir;

    /// Open a real file from disk the way the shell does: the read and the
    /// hash beside it, so the page decides for itself whether it is writable.
    fn open_from_disk(v: &mut CodeView, root: &Path, rel: &str) {
        let path = root.join(rel);
        let read = crate::term::code_git::read_file(&path);
        let hash = crate::term::code_git::file_hash(&path);
        v.set_open(rel.to_string(), read, hash);
    }

    /// Hand the page's own save request to the real writer, the way `app.rs`
    /// does. The test drives both halves of the seam rather than asserting on
    /// the request and calling that a save.
    fn perform(v: &mut CodeView, root: &Path, action: CodeAction) -> Saved {
        let CodeAction::Save(req) = action else {
            panic!("expected a save request, got {action:?}");
        };
        let out = save_file(
            root,
            &req.rel,
            &req.lines,
            req.ending,
            req.trailing_newline,
            req.expect,
        );
        v.set_saved(out.clone());
        out
    }

    #[test]
    fn enter_on_the_body_starts_editing_and_the_header_says_so() {
        let mut v = sample();
        open_file(&mut v, "src/main.rs", &["fn main() {}"]);
        v.body_rows = 8;
        v.focus = Focus::Body;
        assert!(!v.editing(), "opening a file must not start typing into it");
        assert_eq!(
            handle_key(&mut v, key(KeyCode::Enter)),
            CodeAction::FocusChanged
        );
        assert!(v.editing());
        let (buf, _) = painted(&v, Rect::new(0, 0, 100, 12));
        assert!(
            dump(&buf).join("\n").contains("EDIT"),
            "a page taking letters must say so"
        );
    }

    #[test]
    fn a_locked_buffer_refuses_the_editor_and_gives_the_reason() {
        let mut v = sample();
        // A TAB means the buffer is not the file, so it cannot be written back.
        open_file(&mut v, "src/main.rs", &["\tindented"]);
        v.body_rows = 8;
        v.focus = Focus::Body;
        handle_key(&mut v, key(KeyCode::Enter));
        assert!(!v.editing());
        let notice = v.notice.clone().unwrap_or_default();
        assert!(notice.contains(SANITISED_NOTE), "{notice}");
    }

    #[test]
    fn typing_reaches_the_buffer_and_marks_it_dirty() {
        let mut v = sample();
        editing_at(&mut v, "src/main.rs", &["ab"]);
        for c in ['X', 'Y'] {
            handle_key(&mut v, key(KeyCode::Char(c)));
        }
        let o = v.open.as_ref().unwrap();
        assert_eq!(o.lines[0], "XYab");
        assert_eq!(o.col, 2);
        assert!(o.dirty, "an edited buffer must know it is not on disk");
    }

    #[test]
    fn backspace_joins_lines_and_delete_pulls_the_next_one_up() {
        let mut v = sample();
        editing_at(&mut v, "a.rs", &["one", "two"]);
        handle_key(&mut v, key(KeyCode::Down));
        handle_key(&mut v, key(KeyCode::Backspace));
        assert_eq!(v.open.as_ref().unwrap().lines, vec!["onetwo"]);

        let mut v = sample();
        editing_at(&mut v, "a.rs", &["one", "two"]);
        handle_key(&mut v, key(KeyCode::End));
        handle_key(&mut v, key(KeyCode::Delete));
        assert_eq!(v.open.as_ref().unwrap().lines, vec!["onetwo"]);
    }

    #[test]
    fn shift_arrows_select_and_typing_replaces_the_selection() {
        let mut v = sample();
        editing_at(&mut v, "a.rs", &["abcd"]);
        for _ in 0..2 {
            handle_key(&mut v, KeyEvent::new(KeyCode::Right, KeyModifiers::SHIFT));
        }
        assert_eq!(v.open.as_ref().unwrap().selected_text(), "ab");
        handle_key(&mut v, key(KeyCode::Char('Z')));
        assert_eq!(v.open.as_ref().unwrap().lines[0], "Zcd");
    }

    #[test]
    fn esc_leaves_the_editor_before_it_leaves_the_page() {
        let mut v = sample();
        editing_at(&mut v, "a.rs", &["x"]);
        handle_key(&mut v, key(KeyCode::Char('q')));
        assert_eq!(
            handle_key(&mut v, key(KeyCode::Esc)),
            CodeAction::FocusChanged
        );
        assert!(!v.editing(), "the first Esc leaves the editor");
        assert!(v.dirty(), "and leaves the edits alone");
        // The second Esc is the page, and it is guarded rather than obeyed.
        assert_eq!(
            handle_key(&mut v, key(KeyCode::Esc)),
            CodeAction::FocusChanged
        );
        assert_eq!(v.notice.as_deref(), Some(WARN_UNSAVED));
        assert_eq!(handle_key(&mut v, key(KeyCode::Esc)), CodeAction::Close);
    }

    #[test]
    fn the_unsaved_warning_is_cancelled_by_any_other_key() {
        let mut v = sample();
        editing_at(&mut v, "a.rs", &["x"]);
        handle_key(&mut v, key(KeyCode::Char('q')));
        handle_key(&mut v, key(KeyCode::Esc));
        handle_key(&mut v, key(KeyCode::Esc));
        assert_eq!(v.notice.as_deref(), Some(WARN_UNSAVED));
        // Anything but Esc cancels, and is not also acted on.
        assert_eq!(
            handle_key(&mut v, key(KeyCode::Char('j'))),
            CodeAction::FocusChanged
        );
        assert!(v.armed.is_none());
        assert!(v.dirty(), "the buffer survived the cancelled discard");
    }

    #[test]
    fn opening_another_file_over_an_edited_one_warns_first() {
        let mut v = sample();
        editing_at(&mut v, "README.md", &["x"]);
        handle_key(&mut v, key(KeyCode::Char('q')));
        handle_key(&mut v, key(KeyCode::Esc));
        v.focus = Focus::Tree;
        // README.md is the last node of the sample tree.
        let last = v.visible().len() - 1;
        v.tree_sel = last;
        assert_eq!(
            handle_key(&mut v, key(KeyCode::Enter)),
            CodeAction::FocusChanged
        );
        assert_eq!(v.notice.as_deref(), Some(WARN_UNSAVED));
        assert_eq!(
            handle_key(&mut v, key(KeyCode::Enter)),
            CodeAction::Open("README.md".to_string())
        );
    }

    #[test]
    fn a_multi_line_paste_lands_as_lines() {
        let mut v = sample();
        editing_at(&mut v, "a.rs", &["head|tail"]);
        for _ in 0..4 {
            handle_key(&mut v, key(KeyCode::Right));
        }
        assert_eq!(v.paste_text("one\r\ntwo\rthree"), CodeAction::FocusChanged);
        let o = v.open.as_ref().unwrap();
        assert_eq!(o.lines, vec!["headone", "two", "three|tail"]);
        assert_eq!((o.line, o.col), (2, 5), "the cursor follows the paste");
        assert!(o.dirty);
    }

    #[test]
    fn a_paste_carrying_escape_bytes_is_cleaned_and_says_it_was() {
        let mut v = sample();
        editing_at(&mut v, "a.rs", &[""]);
        v.paste_text("red \u{1b}[31mhere\tand a tab");
        let o = v.open.as_ref().unwrap();
        assert!(
            !o.lines.iter().any(|l| l.contains('\u{1b}')),
            "an ESC reached the buffer: {:?}",
            o.lines
        );
        assert!(o.lines[0].contains("    "), "a TAB is not one cell");
        let notice = v.notice.clone().unwrap_or_default();
        assert!(notice.contains("pasted with changes"), "{notice}");
        assert!(
            o.editable(),
            "a cleaned paste is still the buffer that will be written"
        );
    }

    #[test]
    fn a_paste_with_nowhere_to_land_says_so_rather_than_dropping_it() {
        let mut v = sample();
        assert_eq!(v.paste_text("hello"), CodeAction::FocusChanged);
        assert!(v.notice.is_some(), "a paste into the tree must not vanish");
    }

    #[test]
    fn f4_copies_the_selection_when_there_is_one_and_the_whole_file_otherwise() {
        let mut v = sample();
        editing_at(&mut v, "a.rs", &["ab", "cd"]);
        assert_eq!(
            handle_key(&mut v, key(KeyCode::F(4))),
            CodeAction::Copy("ab\ncd\n".to_string())
        );
        for _ in 0..2 {
            handle_key(&mut v, KeyEvent::new(KeyCode::Right, KeyModifiers::SHIFT));
        }
        assert_eq!(
            handle_key(&mut v, key(KeyCode::F(4))),
            CodeAction::Copy("ab".to_string())
        );
    }

    #[test]
    fn a_save_of_a_locked_buffer_is_refused_with_the_reason() {
        let mut v = sample();
        open_file(&mut v, "a.rs", &["\tx"]);
        assert_eq!(
            handle_key(&mut v, key(KeyCode::F(2))),
            CodeAction::FocusChanged
        );
        let notice = v.notice.clone().unwrap_or_default();
        assert!(notice.contains(SANITISED_NOTE), "{notice}");
    }

    #[test]
    fn a_clean_buffer_says_there_is_nothing_to_save() {
        let mut v = sample();
        editing_at(&mut v, "a.rs", &["x"]);
        assert_eq!(
            handle_key(&mut v, key(KeyCode::F(2))),
            CodeAction::FocusChanged
        );
        assert_eq!(v.notice.as_deref(), Some("no changes to save"));
    }

    #[test]
    fn ctrl_s_is_the_pages_only_chord_and_only_while_editing() {
        let mut v = sample();
        let chord = KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL);
        assert!(
            !takes_key(&v, chord),
            "with nothing being edited the capture toggle keeps its chord"
        );
        editing_at(&mut v, "a.rs", &["x"]);
        assert!(takes_key(&v, chord));
        handle_key(&mut v, key(KeyCode::Char('q')));
        assert!(matches!(handle_key(&mut v, chord), CodeAction::Save(_)));
        // And no other Ctrl chord is ever ours.
        for c in ['c', 'b', 'v'] {
            let k = KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL);
            assert!(!takes_key(&v, k), "Ctrl+{c} was swallowed");
        }
    }

    #[test]
    fn the_save_button_and_the_save_key_ask_for_the_same_write() {
        let area = Rect::new(0, 0, 110, 14);
        let mut v = sample();
        editing_at(&mut v, "a.rs", &["x"]);
        handle_key(&mut v, key(KeyCode::Char('q')));
        let (_, r) = painted(&v, area);
        let rect = r.save.expect("a wide pane draws [Save]");
        let mut by_click = v.clone();
        let hit = click(&by_click, area, rect.x, rect.y).expect("the button is hittable");
        assert_eq!(hit, CodeClick::Save);
        let a = act(&mut by_click, hit);
        let b = handle_key(&mut v, key(KeyCode::F(2)));
        assert_eq!(a, b);
        assert!(matches!(a, CodeAction::Save(_)));
    }

    // -- The round trip, against real files on a real disk --------------------

    #[test]
    fn a_crlf_file_saved_from_the_page_is_byte_identical() {
        let td = TempDir::new().unwrap();
        let original: &[u8] = b"line one\r\nline two\r\n";
        std::fs::write(td.path().join("crlf.txt"), original).unwrap();
        let mut v = CodeView::new(td.path().to_path_buf(), Vec::new());
        open_from_disk(&mut v, td.path(), "crlf.txt");
        v.body_rows = 8;
        v.focus = Focus::Body;
        handle_key(&mut v, key(KeyCode::Enter));
        assert!(v.editing(), "a plain CRLF file must be editable");
        // An edit and its exact undo: the buffer is dirty, the bytes are not.
        handle_key(&mut v, key(KeyCode::Char('Z')));
        handle_key(&mut v, key(KeyCode::Backspace));
        assert!(v.dirty());
        let action = handle_key(&mut v, key(KeyCode::F(2)));
        assert!(matches!(perform(&mut v, td.path(), action), Saved::Ok(_)));
        assert_eq!(
            std::fs::read(td.path().join("crlf.txt")).unwrap(),
            original,
            "a CRLF working tree must not be normalised by having been opened"
        );
        assert!(!v.dirty(), "a written buffer is no longer dirty");
    }

    #[test]
    fn a_crlf_file_edited_from_the_page_keeps_crlf_on_the_new_line_too() {
        let td = TempDir::new().unwrap();
        std::fs::write(td.path().join("crlf.txt"), b"one\r\ntwo\r\n").unwrap();
        let mut v = CodeView::new(td.path().to_path_buf(), Vec::new());
        open_from_disk(&mut v, td.path(), "crlf.txt");
        v.body_rows = 8;
        v.focus = Focus::Body;
        handle_key(&mut v, key(KeyCode::Enter));
        handle_key(&mut v, key(KeyCode::End));
        handle_key(&mut v, key(KeyCode::Enter));
        handle_key(&mut v, key(KeyCode::Char('X')));
        let action = handle_key(&mut v, key(KeyCode::F(2)));
        assert!(matches!(perform(&mut v, td.path(), action), Saved::Ok(_)));
        assert_eq!(
            std::fs::read(td.path().join("crlf.txt")).unwrap(),
            b"one\r\nX\r\ntwo\r\n",
            "the line the editor added must carry the file's own terminator"
        );
    }

    #[test]
    fn a_file_with_no_final_newline_does_not_gain_one() {
        let td = TempDir::new().unwrap();
        std::fs::write(td.path().join("bare.txt"), b"one\ntwo").unwrap();
        let mut v = CodeView::new(td.path().to_path_buf(), Vec::new());
        open_from_disk(&mut v, td.path(), "bare.txt");
        v.body_rows = 8;
        v.focus = Focus::Body;
        handle_key(&mut v, key(KeyCode::Enter));
        handle_key(&mut v, key(KeyCode::Char('Z')));
        handle_key(&mut v, key(KeyCode::Backspace));
        let action = handle_key(&mut v, key(KeyCode::F(2)));
        assert!(matches!(perform(&mut v, td.path(), action), Saved::Ok(_)));
        assert_eq!(
            std::fs::read(td.path().join("bare.txt")).unwrap(),
            b"one\ntwo"
        );
    }

    #[test]
    fn a_file_rewritten_underneath_is_refused_untouched_and_the_page_says_so() {
        let td = TempDir::new().unwrap();
        std::fs::write(td.path().join("a.txt"), b"mine\n").unwrap();
        let mut v = CodeView::new(td.path().to_path_buf(), Vec::new());
        open_from_disk(&mut v, td.path(), "a.txt");
        v.body_rows = 8;
        v.focus = Focus::Body;
        handle_key(&mut v, key(KeyCode::Enter));
        handle_key(&mut v, key(KeyCode::Char('Z')));
        std::fs::write(td.path().join("a.txt"), b"somebody else\n").unwrap();
        let action = handle_key(&mut v, key(KeyCode::F(2)));
        assert_eq!(perform(&mut v, td.path(), action), Saved::ChangedOnDisk);
        assert_eq!(
            std::fs::read(td.path().join("a.txt")).unwrap(),
            b"somebody else\n",
            "the other write must survive"
        );
        assert!(v.dirty(), "a refused save leaves the buffer dirty");
        let notice = v.notice.clone().unwrap_or_default();
        assert!(notice.contains("changed on disk"), "{notice}");
    }

    #[test]
    fn a_file_with_mixed_terminators_is_read_only_rather_than_rewritten() {
        let td = TempDir::new().unwrap();
        // Two CRLF lines and one LF: no single terminator writes this back.
        std::fs::write(td.path().join("mixed.txt"), b"one\r\ntwo\nthree\r\n").unwrap();
        let mut v = CodeView::new(td.path().to_path_buf(), Vec::new());
        open_from_disk(&mut v, td.path(), "mixed.txt");
        let o = v.open.as_ref().unwrap();
        assert_eq!(o.locked, Some(NOT_EXACT_NOTE));
        assert!(!o.editable());
        assert!(
            o.hash.is_none(),
            "a buffer with no exact baseline has no hash"
        );
    }

    /// The certification the plan asks for: a **real** CRLF file out of this
    /// repository's working tree, through the page's model, written into a
    /// temporary directory, compared byte for byte.
    #[test]
    fn a_real_crlf_file_from_this_repository_round_trips_byte_for_byte() {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("crates/emma sits two levels under the repository root")
            .to_path_buf();
        // Whichever of these this checkout actually has. Naming one file was
        // the first version of this test and it was a false receipt within the
        // hour: `docs/terminal-pages.html` was CRLF in the index and LF on
        // disk, the `if` below returned, and the test passed having certified
        // nothing. It fails now if it cannot find a real file to work on.
        let candidates = [
            "docs/architecture.html",
            "docs/index.html",
            "docs/records.html",
            ".github/workflows/build.yml",
            "CHANGELOG.md",
        ];
        let (name, bytes) = candidates
            .iter()
            .find_map(|rel| std::fs::read(repo.join(rel)).ok().map(|b| (*rel, b)))
            .expect("this repository must have at least one of its own files");
        // A tree checked out with LF endings still certifies the CRLF path:
        // the content is a real file of real size, given the terminator this
        // box's own checkout would have handed it.
        let original: Vec<u8> = if bytes.windows(2).any(|w| w == b"\r\n") {
            bytes
        } else {
            String::from_utf8(bytes)
                .expect("a text file")
                .replace("\r\n", "\n")
                .replace('\n', "\r\n")
                .into_bytes()
        };
        assert!(
            original.len() > 1000,
            "{name} is too small to certify anything"
        );
        let td = TempDir::new().unwrap();
        std::fs::write(td.path().join("page.html"), &original).unwrap();
        let mut v = CodeView::new(td.path().to_path_buf(), Vec::new());
        open_from_disk(&mut v, td.path(), "page.html");
        let o = v.open.as_ref().expect("the file must open");
        assert_eq!(o.ending, LineEnding::CrLf, "a real CRLF file reads as CRLF");
        assert!(
            o.locked.is_none(),
            "a real source file must be writable: {:?}",
            o.locked
        );
        v.body_rows = 20;
        v.focus = Focus::Body;
        handle_key(&mut v, key(KeyCode::Enter));
        assert!(v.editing());
        handle_key(&mut v, key(KeyCode::Char('Z')));
        handle_key(&mut v, key(KeyCode::Backspace));
        let action = handle_key(&mut v, key(KeyCode::F(2)));
        assert!(matches!(perform(&mut v, td.path(), action), Saved::Ok(_)));
        assert_eq!(
            std::fs::read(td.path().join("page.html")).unwrap(),
            original,
            "{} bytes of {name} did not survive the round trip",
            original.len()
        );
    }

    // -- The column budget, with a cursor over it -----------------------------

    /// One row of the buffer as the **terminal** would show it, not as `dump`
    /// shows it. A two-column glyph occupies the cell after its own, and
    /// ratatui leaves that cell holding whatever was there before, so counting
    /// every cell's symbol overcounts a CJK row by one column per glyph. This
    /// walks the row the way a terminal does — a wide glyph eats the next cell
    /// — which is what makes the assertion below about the screen rather than
    /// about the buffer's bookkeeping.
    fn terminal_row(buf: &Buffer, y: u16) -> String {
        let mut out = String::new();
        let mut x = 0u16;
        while x < buf.area.width {
            let sym = buf[(x, y)].symbol().to_string();
            let w = cols(&sym).max(1) as u16;
            out.push_str(&sym);
            x += w;
        }
        out
    }

    /// Regression class three, with the cursor stage a did not have. A TAB is
    /// four spaces by the time it reaches a cell and a CJK glyph is two
    /// columns, and neither may put a character past the pane's right edge.
    #[test]
    fn a_tab_and_a_wide_glyph_stay_inside_the_pane() {
        let area = Rect::new(0, 0, 46, 10);
        let mut v = sample();
        let wide = "日本語のコードとタブ\tのある行です日本語のコード";
        open_file(&mut v, "wide.rs", &[wide, "plain"]);
        let (buf, r) = painted(&v, area);
        for y in 0..buf.area.height {
            let row = terminal_row(&buf, y);
            assert!(
                cols(&row) <= area.width as usize,
                "row {y} spent {} columns in a {} pane: {row:?}",
                cols(&row),
                area.width
            );
        }
        assert!(!dump(&buf).join("").contains('\t'), "a TAB reached a cell");
        // And nothing wide straddles the document pane's own right border.
        let inner = Block::bordered().inner(r.body);
        for y in 0..buf.area.height {
            let last = buf[(inner.right() - 1, y)].symbol().to_string();
            assert!(cols(&last) <= 1, "a wide glyph straddles the border at {y}");
        }
        // The border itself survives on every row: a glyph that overran would
        // have taken it.
        for y in 1..buf.area.height - 1 {
            assert_eq!(
                buf[(area.width - 1, y)].symbol(),
                "│",
                "the pane's right border was overwritten on row {y}"
            );
        }
    }

    /// The horizontal shift is in **display columns**, and this is the test
    /// that knows the difference. A line of CJK is twice as many columns as
    /// characters, so a shift measured in characters leaves the cursor off the
    /// right of the pane and draws the middle of the line instead of its end.
    ///
    /// It exists because the obvious column test — "no row exceeds the pane
    /// width" — turned out to be a false receipt: ratatui truncates a span at
    /// the rect edge on its own, so deleting this module's own budget check
    /// left that test green. What only this module can get wrong is *which*
    /// part of the line it decided to show.
    #[test]
    fn a_wide_line_shifts_by_columns_so_the_end_of_it_is_reachable() {
        let area = Rect::new(0, 0, 60, 10);
        let mut v = sample();
        let line = format!("{}END", "日".repeat(30));
        editing_at(&mut v, "wide.rs", &[&line]);
        handle_key(&mut v, key(KeyCode::End));
        let g = doc_geom(area, &v).expect("the document is on screen");
        let text_w = (g.area.width - g.gutter) as usize;
        assert_eq!(
            g.hcols,
            cols(&line) + 1 - text_w,
            "the shift must be measured in columns, not characters"
        );
        let (buf, _) = painted(&v, area);
        let row = terminal_row(&buf, g.area.y);
        assert!(
            row.contains("END"),
            "the cursor is at the end of the line and the end is not on screen: {row:?}"
        );
    }

    /// Nothing on a keyboard produces a control character as `KeyCode::Char`,
    /// which is exactly why this is worth pinning: the buffer is the thing a
    /// redirected stream eventually gets, and the one door into it must refuse
    /// the byte class this whole page is organised around refusing.
    #[test]
    fn a_control_character_never_reaches_the_buffer_however_it_arrives() {
        let mut v = sample();
        editing_at(&mut v, "a.rs", &["ab"]);
        for c in ['\u{1b}', '\u{7}', '\u{0}'] {
            handle_key(&mut v, key(KeyCode::Char(c)));
        }
        let o = v.open.as_ref().unwrap();
        assert_eq!(
            o.lines[0], "ab",
            "a control character was typed into the file"
        );
        assert!(
            !o.dirty,
            "and it must not even have marked the buffer dirty"
        );
    }

    /// The conversion stage a did not have to make. `cell_to_pos` and the
    /// paint must agree about which character a column belongs to, or a press
    /// puts the cursor somewhere the person did not point.
    #[test]
    fn a_press_lands_on_the_character_that_was_drawn_there() {
        let area = Rect::new(0, 0, 60, 10);
        let mut v = sample();
        open_file(&mut v, "wide.rs", &["日本語abc"]);
        v.body_rows = 6;
        let g = doc_geom(area, &v).expect("the document is on screen");
        let text_x = g.area.x + g.gutter;
        // Columns 0 and 1 are the first wide glyph; 2 and 3 the second.
        assert_eq!(cell_to_pos(&v, area, text_x, g.area.y), Some((0, 0)));
        assert_eq!(cell_to_pos(&v, area, text_x + 1, g.area.y), Some((0, 0)));
        assert_eq!(cell_to_pos(&v, area, text_x + 2, g.area.y), Some((0, 1)));
        // Six columns in is the fourth character, the first ASCII one.
        assert_eq!(cell_to_pos(&v, area, text_x + 6, g.area.y), Some((0, 3)));
        // A press on the tree is not a press in the document.
        assert_eq!(cell_to_pos(&v, area, 2, g.area.y), None);
    }

    #[test]
    fn a_press_in_the_document_moves_the_cursor_without_starting_to_type() {
        let area = Rect::new(0, 0, 60, 10);
        let mut v = sample();
        open_file(&mut v, "a.rs", &["abcdef", "ghijkl"]);
        v.body_rows = 6;
        let g = doc_geom(area, &v).unwrap();
        let hit = click(&v, area, g.area.x + g.gutter + 3, g.area.y + 1).unwrap();
        assert_eq!(hit, CodeClick::Doc(1, 3));
        act(&mut v, hit);
        let o = v.open.as_ref().unwrap();
        assert_eq!((o.line, o.col), (1, 3));
        assert!(
            !v.editing(),
            "a click is how somebody selects, not how they type"
        );
    }

    #[test]
    fn a_drag_over_the_document_selects_and_the_release_copies_it() {
        let area = Rect::new(0, 0, 60, 10);
        let mut v = sample();
        open_file(&mut v, "a.rs", &["abcdef"]);
        v.body_rows = 6;
        let g = doc_geom(area, &v).unwrap();
        v.press_doc(0, 1);
        let (l, c) = cell_to_pos(&v, area, g.area.x + g.gutter + 4, g.area.y).unwrap();
        v.drag_doc(l, c);
        assert_eq!(v.copy_selection(), CodeAction::Copy("bcd".to_string()));
    }

    #[test]
    fn the_cursor_stays_on_screen_when_it_walks_off_the_bottom() {
        let lines: Vec<String> = (0..60).map(|i| format!("line {i}")).collect();
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        let mut v = sample();
        editing_at(&mut v, "big.rs", &refs);
        for _ in 0..30 {
            handle_key(&mut v, key(KeyCode::Down));
        }
        let o = v.open.as_ref().unwrap();
        assert_eq!(o.line, 30);
        assert!(
            o.line >= o.scroll && o.line < o.scroll + v.body_rows,
            "the cursor left the window: line {} scroll {} rows {}",
            o.line,
            o.scroll,
            v.body_rows
        );
    }

    #[test]
    fn a_narrow_pane_drops_the_save_button_before_it_drops_the_tabs() {
        let mut v = sample();
        editing_at(&mut v, "a.rs", &["x"]);
        let (_, wide) = painted(&v, Rect::new(0, 0, 120, 10));
        assert!(wide.save.is_some() && wide.copy.is_some() && wide.editor.is_some());
        let narrow_area = Rect::new(0, 0, 46, 10);
        let (_, narrow) = painted(&v, narrow_area);
        assert!(
            narrow.file_tab.is_some(),
            "the tabs are the last thing to go"
        );
        assert!(
            narrow.save.is_none(),
            "a button that does not fit must not be drawn"
        );
        // Whatever survives stays inside the bordered pane.
        let inner = Block::bordered().inner(split(narrow_area).body);
        for rect in [narrow.save, narrow.copy, narrow.editor, narrow.file_tab]
            .into_iter()
            .flatten()
        {
            assert!(
                rect.x >= inner.x && rect.right() <= inner.right(),
                "{rect:?} escapes {inner:?}"
            );
        }
    }

    // ---------------------------------------------------------------------
    // Stage c — the chat strip
    // ---------------------------------------------------------------------

    /// The page with a file open and the strip focused, the way three keys
    /// reach it: open, Tab to the body, Tab to the strip.
    fn stripped(v: &mut CodeView, path: &str, lines: &[&str]) {
        open_file(v, path, lines);
        v.body_rows = 10;
        v.focus = Focus::Tree;
        assert_eq!(handle_key(v, key(KeyCode::Tab)), CodeAction::FocusChanged);
        assert_eq!(v.focus, Focus::Body);
        assert_eq!(handle_key(v, key(KeyCode::Tab)), CodeAction::FocusChanged);
    }

    fn type_into_strip(v: &mut CodeView, text: &str) {
        for c in text.chars() {
            assert_eq!(
                handle_key(v, key(KeyCode::Char(c))),
                CodeAction::FocusChanged
            );
        }
    }

    #[test]
    fn tab_reaches_the_strip_and_esc_gives_the_keyboard_back() {
        let mut v = sample();
        stripped(&mut v, "src/main.rs", &["one", "two"]);
        assert_eq!(v.focus, Focus::Chat, "Tab must reach the strip");
        // Esc goes back to the document, not on round the cycle and not out
        // of the page: leaving a text box puts the keyboard where it was.
        assert_eq!(
            handle_key(&mut v, key(KeyCode::Esc)),
            CodeAction::FocusChanged
        );
        assert_eq!(v.focus, Focus::Body);
        // And the page is still open — an Esc that closed it would have thrown
        // away the half-typed question with it.
        assert!(v.open.is_some());
    }

    #[test]
    fn the_strip_is_reachable_from_history_too_and_esc_returns_to_the_patch() {
        let mut v = sample();
        open_file(&mut v, "src/main.rs", &["one"]);
        v.mode = Mode::History;
        v.focus = Focus::Diff;
        assert_eq!(
            handle_key(&mut v, key(KeyCode::Tab)),
            CodeAction::FocusChanged
        );
        assert_eq!(v.focus, Focus::Chat);
        handle_key(&mut v, key(KeyCode::Esc));
        assert_eq!(v.focus, Focus::Diff);
    }

    #[test]
    fn tab_skips_the_strip_when_the_paint_had_no_room_for_it() {
        let mut v = sample();
        open_file(&mut v, "src/main.rs", &["one"]);
        // What a short pane reports.
        let (_, r) = painted(&v, Rect::new(0, 0, 100, 10));
        assert!(
            r.strip.is_none(),
            "a ten-row page has no room for the strip"
        );
        v.strip_shown = r.strip.is_some();
        v.focus = Focus::Body;
        handle_key(&mut v, key(KeyCode::Tab));
        assert_eq!(
            v.focus,
            Focus::Tree,
            "Tab must not focus a box that is not on screen"
        );
        // And on a pane that does have room, the same key reaches it.
        let (_, tall) = painted(&v, Rect::new(0, 0, 100, 24));
        assert!(tall.strip.is_some());
        v.strip_shown = true;
        v.focus = Focus::Body;
        handle_key(&mut v, key(KeyCode::Tab));
        assert_eq!(v.focus, Focus::Chat);
    }

    #[test]
    fn a_submitted_question_leaves_the_page_as_ask_carrying_what_was_typed() {
        let mut v = sample();
        stripped(&mut v, "src/main.rs", &["fn main() {}", "// tail"]);
        type_into_strip(&mut v, "what does this do?");
        assert_eq!(v.strip.input, "what does this do?");
        let action = handle_key(&mut v, key(KeyCode::Enter));
        let CodeAction::Ask(line) = action else {
            panic!("Enter in the strip must produce an Ask, got {action:?}");
        };
        assert!(
            line.ends_with("what does this do?"),
            "the question is the last thing on the line: {line}"
        );
        // The file is named and its text rides along, which is the whole
        // point: an answer about a file emma was never shown is the failure.
        assert!(line.contains("About `src/main.rs`"), "{line}");
        assert!(line.contains("fn main() {}"), "{line}");
        // The box empties, and the pointer row now names what went.
        assert!(v.strip.input.is_empty());
        assert_eq!(v.strip.cursor, 0);
        assert_eq!(v.strip.sent.as_deref(), Some("what does this do?"));
        assert!(v.notice.as_deref().unwrap().contains("transcript"));
    }

    #[test]
    fn an_empty_line_submits_nothing_at_all() {
        let mut v = sample();
        stripped(&mut v, "src/main.rs", &["one"]);
        assert_eq!(handle_key(&mut v, key(KeyCode::Enter)), CodeAction::None);
        assert!(
            v.strip.sent.is_none(),
            "nothing was asked, so nothing was sent"
        );
        // Whitespace is not a question either, and the box keeps it rather
        // than silently clearing what somebody typed.
        type_into_strip(&mut v, "   ");
        assert_eq!(handle_key(&mut v, key(KeyCode::Enter)), CodeAction::None);
        assert!(v.strip.sent.is_none());
        assert_eq!(v.strip.input, "   ");
    }

    #[test]
    fn a_question_with_no_file_open_says_so_rather_than_asking_about_nothing() {
        let mut v = sample();
        v.strip_shown = true;
        v.focus = Focus::Chat;
        type_into_strip(&mut v, "why?");
        assert_eq!(
            handle_key(&mut v, key(KeyCode::Enter)),
            CodeAction::FocusChanged
        );
        assert!(v.strip.sent.is_none());
        assert_eq!(v.notice.as_deref(), Some("open a file to ask about it"));
    }

    #[test]
    fn a_question_about_an_unreadable_file_is_refused_in_words() {
        let mut v = sample();
        v.set_open(
            "a.bin".to_string(),
            FileRead::Refused("binary file".to_string()),
            None,
        );
        v.focus = Focus::Chat;
        type_into_strip(&mut v, "why?");
        assert_eq!(
            handle_key(&mut v, key(KeyCode::Enter)),
            CodeAction::FocusChanged
        );
        assert!(v.strip.sent.is_none());
        assert!(v.notice.as_deref().unwrap().contains("cannot be read"));
    }

    #[test]
    fn a_key_the_editor_owns_is_not_eaten_while_the_editor_has_focus() {
        let mut v = sample();
        editing_at(&mut v, "src/main.rs", &["ab"]);
        v.strip_shown = true;
        // Every key the strip would have taken, aimed at the editor.
        v.open.as_mut().unwrap().col = 2;
        handle_key(&mut v, key(KeyCode::Char('x')));
        handle_key(&mut v, key(KeyCode::Backspace));
        handle_key(&mut v, key(KeyCode::Enter));
        handle_key(&mut v, key(KeyCode::Home));
        assert_eq!(
            v.open.as_ref().unwrap().lines,
            vec!["ab".to_string(), String::new()],
            "the letters and the newline went into the document"
        );
        assert!(
            v.strip.input.is_empty(),
            "the strip must not see a key the editor has: {:?}",
            v.strip.input
        );
        assert!(
            v.strip.sent.is_none(),
            "Enter in the editor is a newline, never a question"
        );
    }

    #[test]
    fn the_strip_did_not_widen_the_predicate_the_shell_shares() {
        let mut v = sample();
        let ctrl_s = KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL);
        editing_at(&mut v, "src/main.rs", &["one"]);
        assert!(
            takes_key(&v, ctrl_s),
            "the editor still owns the save chord"
        );
        // With the strip focused the chord is *not* the page's: it goes back
        // to the capture toggle, exactly as it does over the tree. F2 is the
        // save key that works from anywhere, which is why there are two.
        v.strip_shown = true;
        v.focus = Focus::Chat;
        assert!(
            !takes_key(&v, ctrl_s),
            "a second text box must not add a second Ctrl chord"
        );
        assert!(
            takes_key(&v, key(KeyCode::Char('s'))),
            "plain keys are the page's"
        );
        assert!(!takes_key(
            &v,
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::ALT)
        ));
    }

    #[test]
    fn f2_still_saves_from_inside_the_strip() {
        let mut v = sample();
        editing_at(&mut v, "src/main.rs", &["one"]);
        handle_key(&mut v, key(KeyCode::Char('Z')));
        assert!(v.dirty());
        v.strip_shown = true;
        v.focus = Focus::Chat;
        type_into_strip(&mut v, "hi");
        let action = handle_key(&mut v, key(KeyCode::F(2)));
        assert!(
            matches!(action, CodeAction::Save(_)),
            "F2 is the save key that reaches the buffer from anywhere: {action:?}"
        );
        assert_eq!(
            v.strip.input, "hi",
            "the save must not disturb the question"
        );
    }

    #[test]
    fn the_strip_edits_one_line_the_way_a_one_line_box_does() {
        let mut v = sample();
        stripped(&mut v, "src/main.rs", &["one"]);
        type_into_strip(&mut v, "abc");
        assert_eq!(v.strip.cursor, 3);
        handle_key(&mut v, key(KeyCode::Left));
        handle_key(&mut v, key(KeyCode::Left));
        type_into_strip(&mut v, "X");
        assert_eq!(v.strip.input, "aXbc");
        handle_key(&mut v, key(KeyCode::Home));
        assert_eq!(v.strip.cursor, 0);
        handle_key(&mut v, key(KeyCode::Delete));
        assert_eq!(v.strip.input, "Xbc");
        handle_key(&mut v, key(KeyCode::End));
        handle_key(&mut v, key(KeyCode::Backspace));
        assert_eq!(v.strip.input, "Xb");
        assert_eq!(v.strip.cursor, 2);
    }

    #[test]
    fn a_control_character_never_reaches_the_question() {
        let mut v = sample();
        stripped(&mut v, "src/main.rs", &["one"]);
        // Nothing on a keyboard produces one as a `Char`, but this is the one
        // door into a string that leaves the program.
        for c in ['\u{1b}', '\r', '\u{7}'] {
            handle_key(&mut v, key(KeyCode::Char(c)));
        }
        type_into_strip(&mut v, "ok");
        assert_eq!(v.strip.input, "ok");
        let CodeAction::Ask(line) = handle_key(&mut v, key(KeyCode::Enter)) else {
            panic!("expected an Ask");
        };
        assert!(
            !line.contains('\u{1b}'),
            "an escape byte must not ride out on a composed line"
        );
    }

    #[test]
    fn a_file_too_large_to_send_whole_names_the_lines_that_went() {
        let big: Vec<String> = (0..4000)
            .map(|i| format!("line {i} aaaaaaaaaaaaaaaaaaaa"))
            .collect();
        let refs: Vec<&str> = big.iter().map(String::as_str).collect();
        let mut v = sample();
        open_file(&mut v, "big.rs", &refs);
        v.body_rows = 20;
        v.open.as_mut().unwrap().scroll = 100;
        v.strip_shown = true;
        v.focus = Focus::Chat;
        type_into_strip(&mut v, "why?");
        let CodeAction::Ask(line) = handle_key(&mut v, key(KeyCode::Enter)) else {
            panic!("expected an Ask");
        };
        assert!(
            line.contains("lines 101-120 of 4000"),
            "the window must name itself: {}",
            &line[..line.len().min(160)]
        );
        assert!(
            line.contains("line 100 "),
            "the window is what is on screen"
        );
        assert!(!line.contains("line 3999 "), "the rest must not be in it");
        assert!(
            line.len() < MAX_ASK_BODY + 512,
            "the cap must bound the line: {}",
            line.len()
        );
    }

    #[test]
    fn a_question_about_an_edited_buffer_says_the_unsaved_edits_went_too() {
        let mut v = sample();
        editing_at(&mut v, "src/main.rs", &["one"]);
        handle_key(&mut v, key(KeyCode::Char('Z')));
        assert!(v.dirty());
        v.strip_shown = true;
        v.focus = Focus::Chat;
        type_into_strip(&mut v, "why?");
        let CodeAction::Ask(line) = handle_key(&mut v, key(KeyCode::Enter)) else {
            panic!("expected an Ask");
        };
        assert!(line.contains("with unsaved edits"), "{line}");
        assert!(
            line.contains("Zone"),
            "the buffer is what went, not the file: {line}"
        );
        assert!(v.notice.as_deref().unwrap().contains("unsaved edits"));
    }

    #[test]
    fn a_sanitised_buffer_says_it_is_not_byte_for_byte_the_file() {
        let mut v = sample();
        // A tab is what the read had to change; the hash of the *unchanged*
        // bytes is what the shell hands over, so the buffer locks.
        let read = text(&["a\tb"]);
        let hash = disk(&read);
        v.set_open("t.rs".to_string(), read, hash);
        assert_eq!(v.open.as_ref().unwrap().locked, Some(SANITISED_NOTE));
        v.strip_shown = true;
        v.focus = Focus::Chat;
        type_into_strip(&mut v, "why?");
        let CodeAction::Ask(line) = handle_key(&mut v, key(KeyCode::Enter)) else {
            panic!("expected an Ask");
        };
        assert!(
            line.contains("not byte-for-byte the file"),
            "a buffer that is not the file must say so: {line}"
        );
    }

    #[test]
    fn opening_another_file_drops_the_pointer_to_the_last_files_question() {
        let mut v = sample();
        stripped(&mut v, "src/main.rs", &["one"]);
        type_into_strip(&mut v, "why?");
        handle_key(&mut v, key(KeyCode::Enter));
        assert!(v.strip.sent.is_some());
        open_file(&mut v, "README.md", &["other"]);
        assert!(
            v.strip.sent.is_none(),
            "a pointer naming a question about another file is a sentence about the wrong thing"
        );
    }

    #[test]
    fn the_strip_draws_the_file_it_would_ask_about_and_then_what_was_asked() {
        let mut v = sample();
        let area = Rect::new(0, 0, 100, 24);
        open_file(&mut v, "src/main.rs", &["one"]);
        let (buf, r) = painted(&v, area);
        let rect = r.strip.expect("a 24-row page has room for the strip");
        let text = dump(&buf).join("\n");
        assert!(text.contains("ask emma about src/main.rs"), "{text}");
        // The input row is the one the hit test is told about.
        assert_eq!(click(&v, area, rect.x + 3, rect.y), Some(CodeClick::Strip));
        assert_eq!(act(&mut v, CodeClick::Strip), CodeAction::FocusChanged);
        assert_eq!(v.focus, Focus::Chat);
        v.strip.sent = Some("why?".to_string());
        let (buf, _) = painted(&v, area);
        assert!(dump(&buf).join("\n").contains("asked: why?"));
    }

    #[test]
    fn the_strip_never_takes_a_cell_the_document_drew() {
        let lines: Vec<String> = (0..200).map(|i| format!("line {i}")).collect();
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        let mut v = sample();
        open_file(&mut v, "big.rs", &refs);
        let area = Rect::new(0, 0, 100, 24);
        let (_, r) = painted(&v, area);
        let strip = r.strip.expect("room for the strip");
        let g = doc_geom(area, &v).expect("a document on screen");
        assert!(
            g.area.bottom() <= strip.y,
            "the document {g:?} must end above the strip {strip:?}"
        );
        // A press on the document's last row is the document's, and a press
        // on every row of the strip is the strip's. The second half is what
        // makes this test about the *cells* rather than about two rectangles
        // that happen to agree.
        let x = g.area.x + g.gutter;
        assert!(matches!(
            click(&v, area, x, g.area.bottom() - 1),
            Some(CodeClick::Doc(_, _))
        ));
        for row in strip.y..strip.bottom() {
            assert_eq!(
                click(&v, area, x, row),
                Some(CodeClick::Strip),
                "row {row} is the strip's"
            );
        }
    }

    #[test]
    fn the_strips_rows_stay_inside_the_pane_at_every_width() {
        let mut v = sample();
        open_file(&mut v, "src/main.rs", &["one"]);
        v.focus = Focus::Chat;
        v.strip.input = "x".repeat(400);
        v.strip.cursor = 400;
        for w in [24u16, 40, 61, 100, 200] {
            let area = Rect::new(0, 0, w, 24);
            let (buf, r) = painted(&v, area);
            let Some(rect) = r.strip else { continue };
            let inner = Block::bordered().inner(split(area).body);
            assert!(
                rect.x >= inner.x && rect.right() <= inner.right(),
                "{rect:?}"
            );
            for y in rect.y..rect.bottom() {
                let row = terminal_row(&buf, y);
                assert!(
                    cols(&row) <= w as usize,
                    "row {y} is {} columns wide in a {w}-column terminal: {row:?}",
                    cols(&row)
                );
            }
        }
    }

    /// Certification, not a fixture: a real file of this repository, read
    /// through the real `code_git::read_file`, asked about through the real
    /// keys, and the composed line checked against the bytes on disk.
    ///
    /// A fixture agrees with its author. The one thing this feature can get
    /// wrong that a fixture would never show is a line that *claims* to carry
    /// a file and carries something else, so the assertion is against the file
    /// itself: its first line, one line from the middle, and — because this
    /// one is far over the cap — the exact range the attribution names.
    #[test]
    fn a_real_file_of_this_repository_is_quoted_by_the_line_range_the_strip_names() {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("crates/emma sits two levels under the repository root")
            .to_path_buf();
        let candidates = [
            "crates/emma/src/term/code.rs",
            "crates/emma/src/term/code_git.rs",
            "docs/architecture.html",
            "CHANGELOG.md",
        ];
        let rel = candidates
            .iter()
            .find(|rel| repo.join(rel).is_file())
            .expect("this repository must have at least one of its own files");
        let on_disk = std::fs::read_to_string(repo.join(rel)).expect("a text file");
        let disk_lines: Vec<&str> = on_disk.lines().collect();
        assert!(
            disk_lines.len() > 400,
            "{rel} is too short to certify the windowing"
        );

        let mut v = CodeView::new(repo.clone(), Vec::new());
        open_from_disk(&mut v, &repo, rel);
        let o = v.open.as_ref().expect("the file must open");
        assert!(o.note.is_none(), "{rel} was refused: {:?}", o.note);
        assert_eq!(o.lines.len(), disk_lines.len(), "every line came through");

        v.body_rows = 20;
        v.open.as_mut().unwrap().scroll = 200;
        v.strip_shown = true;
        v.focus = Focus::Chat;
        type_into_strip(&mut v, "what is this file for?");
        let CodeAction::Ask(line) = handle_key(&mut v, key(KeyCode::Enter)) else {
            panic!("expected an Ask");
        };
        assert!(
            line.ends_with("what is this file for?"),
            "the question is last"
        );
        assert!(
            line.contains(&format!("About `{rel}`")),
            "the file is named"
        );
        if on_disk.len() > MAX_ASK_BODY {
            assert!(
                line.contains("lines 201-220 of"),
                "the window must name itself: {}",
                &line[..line.len().min(200)]
            );
            // The rows the attribution claims are the rows a reader of the
            // file would find there — this is the assertion the whole test
            // exists for.
            for (i, disk) in disk_lines.iter().enumerate().take(220).skip(200) {
                let want = sanitise(disk);
                if want.trim().is_empty() {
                    continue;
                }
                assert!(
                    line.contains(&want),
                    "line {} of {rel} is missing from the window: {want:?}",
                    i + 1
                );
            }
            assert!(
                !line.contains(&sanitise(disk_lines[0])) || disk_lines[0].trim().is_empty(),
                "a line outside the named range must not be in it"
            );
        } else {
            assert!(line.contains("the whole file"), "{line}");
        }
        assert!(
            line.len() < MAX_ASK_BODY + 512,
            "the cap must bound the line: {}",
            line.len()
        );
    }

    // region: The LSP half
    // -----------------------------------------------------------------------
    // The LSP half: decorations, the status row, hover and definition.
    //
    // Every one of these drives `apply_lsp`, which is the only door the bridge
    // has into this page. Nothing here starts a server: what a server actually
    // said is certified in `code_lsp`'s two live cases, and what this page does
    // with what it was told is a property of this file.
    // -----------------------------------------------------------------------

    fn diag(line: usize, a: usize, b: usize, severity: Severity, message: &str) -> Diag {
        Diag {
            line,
            end_line: line,
            start_col: a,
            end_col: b,
            severity,
            message: message.to_string(),
        }
    }

    /// Hand the page a set of diagnostics the way the bridge does.
    fn decorate(v: &mut CodeView, path: &str, items: Vec<Diag>) {
        assert_eq!(
            v.apply_lsp(LspUpdate::Diagnostics {
                path: path.to_string(),
                items,
            }),
            None,
            "diagnostics never ask the shell to open anything"
        );
    }

    /// The gutter's separator cell on the row that draws document line `line`.
    fn gutter_mark(buf: &Buffer, v: &CodeView, area: Rect, line: usize) -> String {
        let g = doc_geom(area, v).expect("the document is on screen");
        let row = g.area.y + (line - g.top) as u16;
        buf[(g.area.x + g.gutter - 1, row)].symbol().to_string()
    }

    /// The whole point of the half: a diagnostic marks the line it is about and
    /// underlines exactly the span the server named, and neither decoration is
    /// anywhere else.
    #[test]
    fn a_diagnostic_marks_the_gutter_and_underlines_the_span_it_named() {
        let area = Rect::new(0, 0, 80, 24);
        let mut v = sample();
        open_file(&mut v, "src/main.rs", &["fn main() {", "    bod();", "}"]);
        decorate(
            &mut v,
            "src/main.rs",
            vec![diag(1, 4, 7, Severity::Error, "cannot find value `bod`")],
        );
        let (buf, _) = painted(&v, area);
        assert_eq!(gutter_mark(&buf, &v, area, 1), "E");
        assert_eq!(
            gutter_mark(&buf, &v, area, 0),
            " ",
            "a line with no diagnostic keeps its blank separator"
        );

        let g = doc_geom(area, &v).expect("on screen");
        let text_x = g.area.x + g.gutter;
        let row = g.area.y + 1;
        let underlined = |x: u16| {
            buf[(x, row)]
                .modifier
                .contains(ratatui::style::Modifier::UNDERLINED)
        };
        for col in 4..7u16 {
            assert!(underlined(text_x + col), "char {col} should be underlined");
        }
        assert!(
            !underlined(text_x + 3) && !underlined(text_x + 7),
            "the underline must stop where the server's span stopped"
        );
    }

    /// A line carrying two grades shows the worse one, because the gutter has
    /// one column and a hint drawn over an error is a page hiding the error.
    #[test]
    fn the_worst_severity_on_a_line_is_the_mark_that_is_drawn() {
        let area = Rect::new(0, 0, 80, 24);
        let mut v = sample();
        open_file(&mut v, "src/main.rs", &["one", "two", "three"]);
        decorate(
            &mut v,
            "src/main.rs",
            vec![
                diag(1, 0, 1, Severity::Hint, "unused"),
                diag(1, 0, 1, Severity::Warning, "suspicious"),
                diag(2, 0, 1, Severity::Info, "note"),
            ],
        );
        let (buf, _) = painted(&v, area);
        assert_eq!(gutter_mark(&buf, &v, area, 1), "W");
        assert_eq!(gutter_mark(&buf, &v, area, 2), "i");
    }

    /// A set that is not about the open file is not drawn dim; it is not drawn.
    /// An answer that arrives after the person opened something else would
    /// otherwise decorate the new file with the old file's problems.
    #[test]
    fn a_diagnostic_set_for_another_file_decorates_nothing() {
        let area = Rect::new(0, 0, 80, 24);
        let mut v = sample();
        open_file(&mut v, "src/main.rs", &["one", "two", "three"]);
        decorate(
            &mut v,
            "src/term/code.rs",
            vec![diag(1, 0, 3, Severity::Error, "not about this file")],
        );
        assert!(v.lsp.diags.is_empty(), "a stale set must not be kept");
        let (buf, _) = painted(&v, area);
        for line in 0..3 {
            assert_eq!(gutter_mark(&buf, &v, area, line), " ", "line {line}");
        }
        assert!(v.diag_at_cursor().is_none());
    }

    /// The paint's own path check, and it is not the same guard as
    /// `apply_lsp`'s.
    ///
    /// `set_open` deliberately leaves `lsp` alone - the bridge is the only
    /// thing that writes it, and a page that cleared the decorations itself
    /// would be asserting something it was never told. So between opening a
    /// second file and the server answering about it, `lsp.diags` still
    /// describes the *first* file, and the only thing standing between those
    /// marks and the new file's lines is the check in `draw_file`.
    ///
    /// The mutation sweep found this: deleting that check left the "another
    /// file" test green, because `apply_lsp` had already refused to store the
    /// stale set. Two guards, one test, and the wrong one covered.
    #[test]
    fn opening_another_file_does_not_inherit_the_last_ones_marks() {
        let area = Rect::new(0, 0, 80, 24);
        let mut v = sample();
        open_file(&mut v, "src/main.rs", &["one", "two", "three"]);
        decorate(
            &mut v,
            "src/main.rs",
            vec![diag(1, 0, 3, Severity::Error, "about the first file")],
        );
        let (buf, _) = painted(&v, area);
        assert_eq!(
            gutter_mark(&buf, &v, area, 1),
            "E",
            "the first file is marked"
        );

        open_file(&mut v, "src/term/code.rs", &["alpha", "beta", "gamma"]);
        assert!(
            !v.lsp.diags.is_empty(),
            "the page must not clear what only the bridge may write"
        );
        let (buf, _) = painted(&v, area);
        for line in 0..3 {
            assert_eq!(
                gutter_mark(&buf, &v, area, line),
                " ",
                "line {line} of the new file wears the old file's mark"
            );
        }
    }

    /// The status row is a row the document gave up, so the geometry the paint
    /// used and the geometry the hit test uses have to agree about it. They are
    /// the same function, and this is the test that says so.
    #[test]
    fn the_status_row_takes_a_row_from_the_document_and_the_hit_test_agrees() {
        let area = Rect::new(0, 0, 80, 24);
        let mut v = sample();
        open_file(&mut v, "src/main.rs", &["one", "two", "three"]);
        let before = doc_geom(area, &v).expect("on screen").area.height;
        assert!(lsp_line(&v).is_none(), "a quiet server takes no row");

        decorate(
            &mut v,
            "src/main.rs",
            vec![diag(1, 0, 3, Severity::Error, "boom")],
        );
        let after = doc_geom(area, &v).expect("on screen").area;
        assert_eq!(
            after.height,
            before - 1,
            "the status row must come out of the document"
        );
        let (buf, _) = painted(&v, area);
        let row = dump(&buf)[after.bottom() as usize].clone();
        assert!(
            row.contains("1 error"),
            "the status row is drawn under the document: {row:?}"
        );
        // And the document's last row is still the document's, while the row
        // under it is not.
        assert!(matches!(
            click(&v, area, after.x + after.width - 1, after.bottom() - 1),
            Some(CodeClick::Doc(..))
        ));
        assert!(
            click(&v, area, after.x + after.width - 1, after.bottom()).is_none(),
            "the status row must not answer as a document cell"
        );
    }

    /// **The column budget, with the thing that makes it matter.** The gutter
    /// mark takes the *separator* column rather than a column of its own, which
    /// is what keeps `geom_for`'s gutter width still when a server answers. A
    /// two-column mark would make the row one column wider than the pane and
    /// `put` would drop the last glyph of every marked line — silently, because
    /// ratatui truncates rather than panicking.
    ///
    /// The line here ends in a wide glyph that lands exactly on the pane's last
    /// two columns, so there is no slack to absorb a mistake.
    #[test]
    fn a_gutter_mark_is_one_column_so_a_marked_line_keeps_its_last_glyph() {
        let area = Rect::new(0, 0, 40, 12);
        let mut v = sample();
        let g = {
            let mut probe = sample();
            open_file(&mut probe, "src/main.rs", &["x", "y"]);
            doc_geom(area, &probe).expect("on screen")
        };
        let text_w = (g.area.width - g.gutter) as usize;
        // Fills the text column exactly, ending in a two-column glyph.
        let line = format!("{}\u{65E5}", "a".repeat(text_w - 2));
        open_file(&mut v, "src/main.rs", &[&line, "y"]);
        decorate(
            &mut v,
            "src/main.rs",
            vec![diag(0, 0, 1, Severity::Error, "boom")],
        );
        let (buf, _) = painted(&v, area);
        assert_eq!(gutter_mark(&buf, &v, area, 0), "E", "the mark is drawn");
        let row = terminal_row(&buf, g.area.y);
        assert!(
            row.contains('\u{65E5}'),
            "the mark pushed the line's last glyph off the pane: {row:?}"
        );
        assert!(
            cols(&row) <= area.width as usize,
            "row spent {} columns in a {} pane: {row:?}",
            cols(&row),
            area.width
        );
    }

    /// C2's open question, closed: the per-glyph budget with a mark in the
    /// gutter and a wide glyph at the right edge. It is the same shape as
    /// `a_tab_and_a_wide_glyph_stay_inside_the_pane` and it is kept separate
    /// because the mark is what C2 expected to make the two numbers differ.
    /// **It does not** — the mark is inside the gutter, so `gutter + text_w`
    /// still equals the pane width, and the report says so rather than claiming
    /// a receipt this test cannot write.
    #[test]
    fn a_marked_gutter_and_a_wide_glyph_at_the_right_edge_stay_inside_the_pane() {
        let area = Rect::new(0, 0, 46, 10);
        let mut v = sample();
        let wide = "\u{65E5}\u{672C}\u{8A9E}\u{306E}\u{30B3}\u{30FC}\u{30C9}\tand more \u{65E5}\u{672C}\u{8A9E}";
        open_file(&mut v, "wide.rs", &[wide, "plain"]);
        decorate(
            &mut v,
            "wide.rs",
            vec![
                diag(0, 0, 4, Severity::Error, "wide line"),
                diag(1, 0, 5, Severity::Warning, "plain line"),
            ],
        );
        let (buf, r) = painted(&v, area);
        assert_eq!(gutter_mark(&buf, &v, area, 0), "E");
        for y in 0..buf.area.height {
            let row = terminal_row(&buf, y);
            assert!(
                cols(&row) <= area.width as usize,
                "row {y} spent {} columns in a {} pane: {row:?}",
                cols(&row),
                area.width
            );
        }
        let inner = Block::bordered().inner(r.body);
        for y in 0..buf.area.height {
            let last = buf[(inner.right() - 1, y)].symbol().to_string();
            assert!(cols(&last) <= 1, "a wide glyph straddles the border at {y}");
        }
        for y in 1..buf.area.height - 1 {
            assert_eq!(
                buf[(area.width - 1, y)].symbol(),
                "\u{2502}",
                "the pane's right border was overwritten on row {y}"
            );
        }
    }

    /// A hover answer is a bordered popup over the document, and it reports
    /// where it landed so a test can say "over the document" rather than
    /// "somewhere in the buffer".
    #[test]
    fn a_hover_answer_is_a_popup_over_the_document() {
        let area = Rect::new(0, 0, 80, 24);
        let mut v = sample();
        open_file(&mut v, "src/main.rs", &["fn main() {}", "second", "third"]);
        v.apply_lsp(LspUpdate::Hover {
            path: "src/main.rs".into(),
            lines: Some(vec!["fn main()".into(), "The entry point.".into()]),
        });
        let (buf, r) = painted(&v, area);
        let rect = r.hover.expect("the popup reports where it landed");
        let content = doc_geom(area, &v).map(|g| g.area).unwrap_or_default();
        assert!(
            rect.x >= content.x && rect.y >= content.y && rect.bottom() <= content.bottom(),
            "the popup must sit over the document, not over the header: {rect:?}"
        );
        let rows = dump(&buf);
        assert!(
            rows[rect.y as usize + 1].contains("fn main()"),
            "{:?}",
            rows[rect.y as usize + 1]
        );
        assert!(rows[rect.y as usize + 2].contains("The entry point."));
    }

    /// The popup is modal: the arrows scroll it and every other key closes it
    /// **without also doing its usual job**, the same rule the unsaved-changes
    /// warning follows. A key that dismissed the overlay and then edited the
    /// line it was covering would be one keystroke doing two things.
    #[test]
    fn the_hover_popup_scrolls_and_any_other_key_closes_it_without_acting() {
        let mut v = sample();
        editing_at(&mut v, "src/main.rs", &["one", "two"]);
        let long: Vec<String> = (0..10).map(|i| format!("line {i}")).collect();
        v.apply_lsp(LspUpdate::Hover {
            path: "src/main.rs".into(),
            lines: Some(long),
        });
        assert_eq!(
            handle_key(&mut v, key(KeyCode::Down)),
            CodeAction::FocusChanged
        );
        assert_eq!(v.lsp.hover.as_ref().expect("still open").scroll, 1);
        assert_eq!(
            handle_key(&mut v, key(KeyCode::Up)),
            CodeAction::FocusChanged
        );
        assert_eq!(v.lsp.hover.as_ref().expect("still open").scroll, 0);

        assert_eq!(
            handle_key(&mut v, key(KeyCode::Char('x'))),
            CodeAction::FocusChanged
        );
        assert!(v.lsp.hover.is_none(), "any other key closes the popup");
        assert_eq!(
            v.open.as_ref().expect("open").lines[0],
            "one",
            "the key that closed the popup must not also have typed into the buffer"
        );
    }

    /// The save chord is above the popup for a reason: a person with an
    /// overlay open and an edited buffer must not have to dismiss the overlay
    /// to save.
    #[test]
    fn the_save_keys_still_reach_the_buffer_with_a_popup_open() {
        let mut v = sample();
        editing_at(&mut v, "src/main.rs", &["one"]);
        handle_key(&mut v, key(KeyCode::Char('x')));
        v.apply_lsp(LspUpdate::Hover {
            path: "src/main.rs".into(),
            lines: Some(vec!["hover".into()]),
        });
        assert!(matches!(
            handle_key(&mut v, key(KeyCode::F(2))),
            CodeAction::Save(_)
        ));
        assert!(v.lsp.hover.is_some(), "saving does not dismiss the popup");
    }

    /// A server with nothing to say said something, and the page repeats it
    /// rather than opening an empty box.
    #[test]
    fn a_hover_with_nothing_in_it_is_a_sentence_rather_than_an_empty_box() {
        let mut v = sample();
        open_file(&mut v, "src/main.rs", &["one"]);
        v.apply_lsp(LspUpdate::Hover {
            path: "src/main.rs".into(),
            lines: None,
        });
        assert!(v.lsp.hover.is_none());
        assert_eq!(
            lsp_line(&v).as_deref(),
            Some("no hover information here"),
            "silence must not be the answer to a key that was pressed"
        );
    }

    /// A definition in the open file is applied here; one in another file is
    /// the single answer this page hands back, because opening a file is a read
    /// and this module never reads.
    #[test]
    fn a_definition_here_jumps_and_one_elsewhere_is_handed_to_the_shell() {
        let mut v = sample();
        open_file(&mut v, "src/main.rs", &["one", "two", "three", "four"]);
        v.body_rows = 4;
        assert_eq!(
            v.apply_lsp(LspUpdate::Definition {
                path: "src/main.rs".into(),
                target: DefTarget::Inside {
                    rel: "src/main.rs".into(),
                    line: 2,
                    col: 1,
                },
            }),
            None
        );
        let o = v.open.as_ref().expect("open");
        assert_eq!((o.line, o.col), (2, 1));
        assert_eq!(lsp_line(&v).as_deref(), Some("jumped to line 3"));

        assert_eq!(
            v.apply_lsp(LspUpdate::Definition {
                path: "src/main.rs".into(),
                target: DefTarget::Inside {
                    rel: "src/term/code.rs".into(),
                    line: 9,
                    col: 4,
                },
            }),
            Some(("src/term/code.rs".to_string(), 9, 4)),
            "a cross-file jump is the shell's, because it is a read"
        );
    }

    /// The colour a reader actually sees, taken off the painted buffer rather
    /// than off `syntax_role`.
    ///
    /// **A test on the mapping alone would pass with the colour never
    /// reaching a cell**, which is the shape of false receipt this project has
    /// paid for before. So this paints the page and reads the styles back: the
    /// comment is dim and the keyword is not, and the two are different
    /// styles, which is the whole of the complaint that started this work.
    #[test]
    fn a_comment_and_the_code_beside_it_are_painted_differently() {
        use emma_tools_lsp::render::{Token, TokenKind};
        let mut v = sample();
        open_file(&mut v, "src/main.rs", &["let x = 1; // why"]);
        v.body_rows = 10;
        v.apply_lsp(LspUpdate::Tokens {
            path: "src/main.rs".into(),
            items: vec![
                Token {
                    line: 0,
                    start: 0,
                    end: 3,
                    kind: TokenKind::Keyword,
                },
                Token {
                    line: 0,
                    start: 11,
                    end: 17,
                    kind: TokenKind::Comment,
                },
            ],
        });
        let area = Rect::new(0, 0, 100, 24);
        let (buf, _) = painted(&v, area);

        // The document's own row, found by its text rather than by a column
        // this test computed: where the gutter ends is not this test's
        // business, and the header carries a `/` of its own in the path, so a
        // search over the whole pane would read the wrong cell and pass or
        // fail for a reason that has nothing to do with syntax colour.
        let rows = dump(&buf);
        let y = rows
            .iter()
            .position(|r| r.contains("let x = 1;"))
            .expect("the document line is drawn") as u16;
        let cell = |glyph: &str| {
            (0..area.width)
                .map(|x| &buf[(x, y)])
                .find(|c| c.symbol() == glyph)
                .unwrap_or_else(|| panic!("no {glyph} on the document's line"))
        };
        let skin = skin();
        let keyword = cell("l");
        let comment = cell("/");
        // A cell no token covers. The second copy of this test, deleted with
        // this line moved into it, checked only that the three differ; this one
        // checks the roles as well, so it subsumes it.
        let plain = cell("x");
        assert_eq!(
            keyword.style().fg,
            skin.palette.style(Role::Keyword).fg,
            "the keyword is drawn in the keyword's colour"
        );
        assert_eq!(
            comment.style().fg,
            skin.palette.style(Role::Comment).fg,
            "the comment is drawn in the comment's colour"
        );
        assert_ne!(
            keyword.style().fg,
            comment.style().fg,
            "a comment the same colour as the code is the defect this fixes"
        );
        assert_ne!(
            keyword.style().fg,
            plain.style().fg,
            "a character no token covers must not take the keyword's colour"
        );
        assert_ne!(comment.style().fg, plain.style().fg, "nor the comment's");
        assert!(
            comment
                .style()
                .add_modifier
                .contains(ratatui::style::Modifier::DIM),
            "and it is dim, which is the half that survives a colourless terminal"
        );
    }

    fn places(rows: Vec<(&str, usize, usize)>) -> LspUpdate {
        LspUpdate::Places {
            path: "src/main.rs".into(),
            about: "references to widget".into(),
            rows: rows
                .into_iter()
                .map(|(rel, line, col)| Place {
                    label: format!("{rel}:{}", line + 1),
                    rel: rel.to_string(),
                    line,
                    col,
                })
                .collect(),
        }
    }

    /// The two list keys reach the shell from the browser and from inside the
    /// document, which is the whole reason they are function keys.
    #[test]
    fn f8_asks_where_a_symbol_is_used_and_f10_what_the_file_contains() {
        let mut v = sample();
        open_file(&mut v, "src/main.rs", &["fn main() {}"]);
        assert_eq!(
            handle_key(&mut v, key(KeyCode::F(8))),
            CodeAction::References
        );
        assert_eq!(handle_key(&mut v, key(KeyCode::F(10))), CodeAction::Symbols);

        let mut v = sample();
        editing_at(&mut v, "src/main.rs", &["fn main() {}"]);
        assert_eq!(
            handle_key(&mut v, key(KeyCode::F(8))),
            CodeAction::References,
            "a letter would be text here; a function key still arrives"
        );
        assert_eq!(handle_key(&mut v, key(KeyCode::F(10))), CodeAction::Symbols);
    }

    /// With nothing readable open they refuse in words, the rule `F5` and `F6`
    /// already follow: a key that silently does nothing is a key people report.
    #[test]
    fn the_list_keys_refuse_in_words_when_there_is_no_file_to_ask_about() {
        let mut v = sample();
        for code in [KeyCode::F(8), KeyCode::F(10)] {
            v.lsp.note = None;
            assert_eq!(handle_key(&mut v, key(code)), CodeAction::FocusChanged);
            assert!(
                lsp_line(&v).is_some_and(|l| l.contains("open a text file first")),
                "{code:?} said nothing"
            );
        }
    }

    /// An empty answer is an answer. It says so on the status row rather than
    /// opening a panel with no rows in it, which would be a box saying nothing.
    #[test]
    fn no_results_is_a_sentence_rather_than_an_empty_panel() {
        let mut v = sample();
        open_file(&mut v, "src/main.rs", &["fn main() {}"]);
        assert_eq!(v.apply_lsp(places(vec![])), None);
        assert!(v.lsp.places.is_none(), "no panel opens");
        let line = lsp_line(&v).expect("a sentence");
        assert!(
            line.contains("no results for references to widget"),
            "{line}"
        );
    }

    /// The panel owns the keyboard while it is up: the arrows move the
    /// selection, Enter opens the chosen row, and anything else closes it
    /// without reaching the buffer underneath.
    #[test]
    fn the_places_panel_moves_opens_and_closes() {
        let mut v = sample();
        editing_at(&mut v, "src/main.rs", &["fn main() {}"]);
        let before = v.open.as_ref().expect("open").lines.clone();

        assert_eq!(
            v.apply_lsp(places(vec![
                ("src/main.rs", 0, 3),
                ("src/term/code.rs", 41, 8),
            ])),
            None
        );
        assert_eq!(
            handle_key(&mut v, key(KeyCode::Down)),
            CodeAction::FocusChanged
        );
        assert_eq!(
            handle_key(&mut v, key(KeyCode::Enter)),
            CodeAction::Goto(Place {
                label: "src/term/code.rs:42".into(),
                rel: "src/term/code.rs".into(),
                line: 41,
                col: 8,
            }),
            "Enter opens the row the arrows landed on"
        );
        assert!(v.lsp.places.is_none(), "opening a row closes the panel");

        // And a key that is not one of the three closes it without editing the
        // line it was drawn over.
        v.apply_lsp(places(vec![("src/main.rs", 0, 3)]));
        assert_eq!(
            handle_key(&mut v, key(KeyCode::Char('x'))),
            CodeAction::FocusChanged
        );
        assert!(v.lsp.places.is_none());
        assert_eq!(
            v.open.as_ref().expect("open").lines,
            before,
            "the key did not reach the buffer"
        );
    }

    /// **The paint and the keyboard agree about which overlay is on top.**
    ///
    /// Both can be set: press `F8`, then `F5` before the references answer
    /// lands. `draw_body` draws the panel and not the hover, and `handle_key`
    /// used to ask the hover first — so the arrows moved nothing, and the first
    /// key silently closed a popup that was never on screen. This asserts the
    /// two orders from the same state, so it fails whichever of them is
    /// reversed.
    #[test]
    fn the_overlay_that_is_drawn_is_the_one_that_takes_the_keys() {
        let mut v = sample();
        open_file(&mut v, "src/main.rs", &["fn main() {}"]);
        v.body_rows = 10;
        v.apply_lsp(places(vec![
            ("src/main.rs", 0, 3),
            ("src/term/code.rs", 41, 8),
        ]));
        v.apply_lsp(LspUpdate::Hover {
            path: "src/main.rs".into(),
            lines: Some(vec!["fn main()".into()]),
        });
        assert!(
            v.lsp.places.is_some() && v.lsp.hover.is_some(),
            "both are up"
        );

        // What the paint chose.
        let area = Rect::new(0, 0, 100, 24);
        let (buf, _) = painted(&v, area);
        let rows = dump(&buf);
        assert!(
            rows.iter().any(|r| r.contains("references to widget (2)")),
            "the panel is the overlay on screen: {rows:#?}"
        );

        // What the keyboard chose. Down moves the panel's selection; if the
        // hover had taken the key the panel would be untouched and the hover
        // scrolled or closed instead.
        handle_key(&mut v, key(KeyCode::Down));
        assert_eq!(
            v.lsp.places.as_ref().map(|p| p.at),
            Some(1),
            "the arrow reached the panel that is on screen"
        );
        assert!(
            v.lsp.hover.is_some(),
            "and did not close the overlay nobody can see"
        );
    }

    /// **An armed discard is answered before any overlay sees the key.**
    ///
    /// A places answer can land in the gap between arming the warning and the
    /// key that answers it. With the panel above the warning, the panel ate the
    /// cancelling key, the warning stayed armed with its notice on screen, and
    /// the *next* `Esc` closed the page and threw the edits away — a
    /// destructive action reached by a key somebody thought was closing a list.
    #[test]
    fn a_panel_cannot_swallow_the_key_that_cancels_a_discard() {
        let mut v = sample();
        editing_at(&mut v, "src/main.rs", &["fn main() {}"]);
        handle_key(&mut v, key(KeyCode::Char('x')));
        assert!(v.dirty(), "there are edits to lose");
        handle_key(&mut v, key(KeyCode::Esc));
        assert_eq!(
            handle_key(&mut v, key(KeyCode::Esc)),
            CodeAction::FocusChanged,
            "the second Esc arms the warning rather than closing the page"
        );
        assert!(v.armed.is_some(), "armed");

        // The answer arrives while the warning waits.
        v.apply_lsp(places(vec![("src/main.rs", 0, 3)]));
        assert_eq!(
            handle_key(&mut v, key(KeyCode::Char('q'))),
            CodeAction::FocusChanged
        );
        assert!(
            v.armed.is_none(),
            "the warning was answered rather than left armed behind a panel"
        );

        // And so the panel's own Esc closes the panel, not the page.
        assert_eq!(
            handle_key(&mut v, key(KeyCode::Esc)),
            CodeAction::FocusChanged
        );
        assert!(v.lsp.places.is_none(), "the panel closed");
        assert!(v.dirty(), "and nothing was discarded");
    }

    /// The overlay's fill, which is the half of it that has no other witness.
    ///
    /// A border rendered over a painted document without clearing the cells
    /// inside it leaves the file showing through the panel, which reads as
    /// corruption rather than as an overlay. Three draw functions shared this
    /// preamble and only one carried the argument for it.
    #[test]
    fn an_overlay_clears_the_document_out_from_under_itself() {
        let mut v = sample();
        // Several lines of one distinctive glyph, each wider than the pane: the
        // panel's own rows must land over painted text, or the fill has nothing
        // to clear and the test cannot fail.
        let long = "Z".repeat(120);
        let lines: Vec<&str> = (0..8).map(|_| long.as_str()).collect();
        open_file(&mut v, "src/main.rs", &lines);
        v.body_rows = 10;
        v.apply_lsp(places(vec![("src/main.rs", 0, 3)]));
        let area = Rect::new(0, 0, 100, 24);
        let (buf, _) = painted(&v, area);
        let rows = dump(&buf);
        let panel = rows
            .iter()
            .find(|r| r.contains("references to widget"))
            .expect("the panel header is drawn");
        // Everything from the header text to the panel's right border. The
        // document's `Z`s are to the *left* of the panel too, which is correct
        // and is why this looks only inside it -- and the border itself is the
        // last glyph on the row, which is why the first version of this test
        // asked whether the row ended in a `Z` and could not fail.
        let inside = &panel[panel.find("references").expect("the header")..];
        assert!(
            !inside.contains('Z'),
            "the document is showing through the panel: {inside:?}"
        );
    }

    /// A span that runs over several lines marks every one of them.
    ///
    /// Every diagnostic fixture on this page was one line high, so the
    /// containment rule the gutter and the underline share could have been
    /// `line == d.line` and nothing would have gone red. rust-analyzer spans
    /// several lines routinely — an unclosed brace, a mismatched block.
    #[test]
    fn a_diagnostic_that_spans_lines_marks_every_line_it_covers() {
        let mut v = sample();
        open_file(&mut v, "src/main.rs", &["fn main() {", "    x", "}"]);
        v.body_rows = 10;
        decorate(
            &mut v,
            "src/main.rs",
            vec![Diag {
                line: 0,
                end_line: 2,
                start_col: 10,
                end_col: 1,
                severity: Severity::Error,
                message: "this block is never closed".into(),
            }],
        );
        let area = Rect::new(0, 0, 100, 24);
        let (buf, _) = painted(&v, area);
        for line in 0..3 {
            assert_eq!(
                gutter_mark(&buf, &v, area, line),
                Severity::Error.mark().to_string(),
                "line {line} of a three-line span is unmarked"
            );
        }
        // And the underline reaches the middle line, which has no endpoint of
        // its own: the span covers it whole.
        assert!(covers(&v.lsp.diags[0], 1, 0), "the middle line is covered");
    }

    /// Drawn: the header says what was asked and how many came back, and the
    /// chosen row is a styled cell rather than a colour, because colour alone
    /// is not a marker on a monochrome terminal.
    #[test]
    fn the_places_panel_is_painted_with_its_count_and_a_marked_row() {
        let mut v = sample();
        open_file(&mut v, "src/main.rs", &["fn main() {}"]);
        v.body_rows = 10;
        v.apply_lsp(places(vec![
            ("src/main.rs", 0, 3),
            ("src/term/code.rs", 41, 8),
        ]));
        let area = Rect::new(0, 0, 100, 24);
        let (buf, _) = painted(&v, area);
        let rows = dump(&buf);
        assert!(
            rows.iter().any(|r| r.contains("references to widget (2)")),
            "the header names the ask and the count: {rows:#?}"
        );
        assert!(
            rows.iter().any(|r| r.contains("src/term/code.rs:42")),
            "every row is drawn: {rows:#?}"
        );
        let chosen = buf
            .content()
            .iter()
            .any(|c| c.symbol() == "s" && c.style() == skin().palette.chip(Role::Accent));
        assert!(chosen, "the chosen row is a styled cell, not a colour");
    }

    /// The containment law, on this side of the seam: a definition outside the
    /// repository is named and nothing opens, because this page's Save writes
    /// inside the root and an editor over a file it cannot save is a trap.
    #[test]
    fn a_definition_outside_the_repository_is_named_and_nothing_opens() {
        let mut v = sample();
        open_file(&mut v, "src/main.rs", &["one"]);
        assert_eq!(
            v.apply_lsp(LspUpdate::Definition {
                path: "src/main.rs".into(),
                target: DefTarget::Outside("C:/src/rust/core/option.rs".into()),
            }),
            None
        );
        let line = lsp_line(&v).expect("a sentence");
        assert!(line.contains("outside this repository"), "{line}");
        assert!(line.contains("option.rs"), "{line}");
        assert_eq!(
            v.open.as_ref().expect("open").path,
            "src/main.rs",
            "nothing may have been opened"
        );
    }

    /// A late answer about a file nobody is looking at any more is dropped, not
    /// drawn. It is the same rule as the diagnostics one and it is separately
    /// worth pinning, because the popup is the decoration a person would
    /// actually read and believe.
    #[test]
    fn an_answer_about_a_file_that_is_no_longer_open_is_dropped() {
        let mut v = sample();
        open_file(&mut v, "src/main.rs", &["one"]);
        v.apply_lsp(LspUpdate::Hover {
            path: "src/term/code.rs".into(),
            lines: Some(vec!["about the other file".into()]),
        });
        v.apply_lsp(LspUpdate::Note {
            path: "src/term/code.rs".into(),
            text: "about the other file".into(),
        });
        assert!(v.lsp.hover.is_none());
        assert_eq!(lsp_line(&v), None, "nothing about the other file is shown");
    }

    /// `F5` and `F6` with nothing readable open refuse in words. A request the
    /// shell could not fill would reach the person as silence, which is the one
    /// answer this page never gives.
    #[test]
    fn f5_and_f6_refuse_in_words_when_there_is_no_readable_file() {
        let mut v = sample();
        assert_eq!(
            handle_key(&mut v, key(KeyCode::F(5))),
            CodeAction::FocusChanged
        );
        assert_eq!(lsp_line(&v).as_deref(), Some("open a text file first"));

        v.set_open(
            "bin".to_string(),
            FileRead::Refused("not a text file".into()),
            None,
        );
        assert_eq!(
            handle_key(&mut v, key(KeyCode::F(6))),
            CodeAction::FocusChanged
        );
        assert!(lsp_line(&v)
            .expect("a sentence")
            .contains("open a text file first"));

        open_file(&mut v, "src/main.rs", &["one"]);
        assert_eq!(handle_key(&mut v, key(KeyCode::F(5))), CodeAction::Hover);
        assert_eq!(
            handle_key(&mut v, key(KeyCode::F(6))),
            CodeAction::Definition
        );
    }

    /// Only errors and warnings are counted. rust-analyzer emits an "inactive
    /// code" hint for every `cfg`-disabled block, so a count that included
    /// hints would report dozens of problems in a file that has none.
    #[test]
    fn the_count_ignores_hints_so_cfg_blocks_do_not_read_as_problems() {
        let mut v = sample();
        open_file(&mut v, "src/main.rs", &["one", "two", "three"]);
        decorate(
            &mut v,
            "src/main.rs",
            vec![
                diag(0, 0, 1, Severity::Hint, "inactive code"),
                diag(1, 0, 1, Severity::Hint, "inactive code"),
            ],
        );
        assert_eq!(v.lsp.count_line(), None);
        decorate(
            &mut v,
            "src/main.rs",
            vec![
                diag(0, 0, 1, Severity::Error, "a"),
                diag(1, 0, 1, Severity::Error, "b"),
                diag(2, 0, 1, Severity::Warning, "c"),
                diag(2, 1, 2, Severity::Hint, "d"),
            ],
        );
        assert_eq!(v.lsp.count_line().as_deref(), Some("2 errors, 1 warning"));
    }

    /// Every kind of "no" is a different sentence, and none of them is
    /// silence: a page with no decorations because the server is indexing and a
    /// page with no decorations because the file is clean must not look alike.
    #[test]
    fn every_kind_of_no_is_its_own_sentence() {
        let cases = [
            (
                LspStatus::Unsupported(".md files".into()),
                "no language server",
            ),
            (LspStatus::Disabled("Terraform".into()), "lsp.enabled"),
            (
                LspStatus::Absent {
                    label: "Bash".into(),
                    detail: "no bash-language-server on PATH".into(),
                },
                "bash-language-server",
            ),
            (LspStatus::Starting("Rust".into()), "starting"),
            (LspStatus::Running("rust-analyzer".into()), "running"),
            (LspStatus::Failed("it died".into()), "stopped"),
        ];
        for (status, want) in cases {
            let mut v = sample();
            open_file(&mut v, "src/main.rs", &["one"]);
            v.apply_lsp(LspUpdate::Status(status.clone()));
            let line = lsp_line(&v).expect("every status but Idle says something");
            assert!(line.contains(want), "{status:?} said {line:?}");
        }
        let mut v = sample();
        v.apply_lsp(LspUpdate::Status(LspStatus::Idle));
        assert_eq!(lsp_line(&v), None, "no file open is not a complaint");
    }

    /// The status row carries the message under the cursor, flattened: a
    /// diagnostic routinely has a second line, and a newline written into a
    /// `Buffer` is a cell rather than a break.
    #[test]
    fn the_message_under_the_cursor_reaches_the_status_row_on_one_line() {
        let area = Rect::new(0, 0, 80, 24);
        let mut v = sample();
        open_file(&mut v, "src/main.rs", &["one", "two", "three"]);
        decorate(
            &mut v,
            "src/main.rs",
            vec![diag(
                1,
                0,
                3,
                Severity::Error,
                "expected one of these\n  - a semicolon\n",
            )],
        );
        v.jump_to(1, 0);
        let line = lsp_line(&v).expect("a message");
        assert!(
            line.contains("E: expected one of these - a semicolon"),
            "{line}"
        );
        assert!(!line.contains('\n'));
        let (buf, _) = painted(&v, area);
        assert!(dump(&buf).iter().any(|r| r.contains("a semicolon")));
        v.jump_to(0, 0);
        assert!(
            !lsp_line(&v)
                .expect("the count survives")
                .contains("semicolon"),
            "the cursor moved off the line, so its message goes with it"
        );
    }

    // endregion: The LSP half

    // -- the completion popup ------------------------------------------------

    fn candidate(label: &str, filter: &str, insert: &str) -> Candidate {
        Candidate {
            label: label.to_string(),
            filter: filter.to_string(),
            insert: insert.to_string(),
            replace: None,
            kind: "method",
            detail: None,
        }
    }

    /// A file with the cursor mid-word, ready for a completion.
    fn with_word(word: &str) -> CodeView {
        let mut v = sample();
        v.body_rows = 10;
        // `editing_at` is the helper that opens a file *editable*: the two
        // hashes agree, so nothing locks the buffer. Passing `None` for the
        // disk hash marks it not-exact and read-only, which is C2's rule
        // working and cost this fixture its first run.
        editing_at(&mut v, "src/lib.rs", &[&format!("fn main() {{ {word}")]);
        let o = v.open.as_mut().expect("open");
        o.line = 0;
        o.col = o.lines[0].chars().count();
        v
    }

    /// **What the popup matches against is the filter, never the label.**
    /// A server labels a method `push(…)` and filters it as `push`; matching
    /// the label is why some editors stop finding anything after the bracket.
    #[test]
    fn the_popup_narrows_on_the_filter_text_and_not_the_label() {
        let mut popup = Popup {
            items: vec![
                candidate("push(…)", "push", "push"),
                candidate("pop(…)", "pop", "pop"),
                candidate("clear(…)", "clear", "clear"),
            ],
            ..Popup::default()
        };
        assert_eq!(popup.visible().len(), 3);
        popup.typed = "p".to_string();
        assert_eq!(popup.visible().len(), 2, "p matches push and pop");
        popup.typed = "pu".to_string();
        assert_eq!(popup.visible().len(), 1);
        assert_eq!(popup.chosen().map(|c| c.filter.as_str()), Some("push"));
        // A bracket appears in every label and in no filter, so a popup
        // matching labels would still show three here.
        popup.typed = "(".to_string();
        assert!(
            popup.visible().is_empty(),
            "the labels were matched instead of the filters"
        );
    }

    /// A prefix beats a substring, and the server's own ranking is kept inside
    /// each group rather than re-sorted: it knows which of two matches is more
    /// likely and this does not.
    #[test]
    fn a_prefix_match_outranks_a_substring_and_the_server_order_survives() {
        let popup = Popup {
            items: vec![
                candidate("a", "with_push", "with_push"),
                candidate("b", "pushed", "pushed"),
                candidate("c", "pushing", "pushing"),
            ],
            typed: "push".to_string(),
            ..Popup::default()
        };
        let seen: Vec<&str> = popup.visible().iter().map(|c| c.filter.as_str()).collect();
        assert_eq!(
            seen,
            vec!["pushed", "pushing", "with_push"],
            "prefixes must come first, and the server's order kept within a group"
        );
    }

    /// Accepting replaces the word under the cursor rather than appending to
    /// it. Appending is the defect that turns `pu` plus `push` into `pupush`.
    #[test]
    fn accepting_replaces_the_word_under_the_cursor() {
        let mut v = with_word("pu");
        v.lsp.popup = Some(Popup {
            items: vec![candidate("push(…)", "push", "push")],
            typed: "pu".to_string(),
            ..Popup::default()
        });
        v.accept_completion();
        assert_eq!(
            v.open.as_ref().unwrap().lines[0],
            "fn main() { push",
            "the typed prefix was not replaced"
        );
        assert!(
            v.lsp.popup.is_none(),
            "the popup stayed open after accepting"
        );
        assert!(v.open.as_ref().unwrap().dirty);
    }

    /// A server-supplied range wins over the word rule, because the server is
    /// the one that knows the language. Here it replaces more than an
    /// identifier would.
    ///
    /// The origin is the cursor, which is the condition the range is good
    /// under: see the test below for what happens when it is not.
    #[test]
    fn a_server_range_wins_over_the_word_before_the_cursor() {
        let mut v = with_word("a.b");
        let mut item = candidate("total", "total", "total");
        // Columns 12..15 are `a.b`, which no identifier rule would take whole.
        item.replace = Some((12, 15));
        let at = (0, v.open.as_ref().expect("open").col);
        v.lsp.popup = Some(Popup {
            items: vec![item],
            origin: at,
            ..Popup::default()
        });
        v.accept_completion();
        assert_eq!(v.open.as_ref().unwrap().lines[0], "fn main() { total");
    }

    /// **And it stops winning the moment the cursor leaves the position it was
    /// computed at**, which is the common case rather than the corner: the list
    /// narrows as somebody types and is not asked again, so by the time they
    /// press Enter they are three letters past where the server answered.
    ///
    /// Certified against rust-analyzer 1.94.1: a completion after a dot comes
    /// back with an *empty* replace range at the request position. Applying
    /// that after three more letters inserts the candidate and leaves the three
    /// behind, so `c.pus` accepted as `push` produced `c.pushpus`. The word
    /// before the cursor is the honest range once the cursor has moved -- the
    /// same fallback a server naming no range already gets.
    #[test]
    fn a_server_range_is_dropped_once_the_cursor_has_moved_past_it() {
        let mut v = with_word("c.pus");
        let mut item = candidate("push", "push", "push");
        // What rust-analyzer actually sends: an empty range at the point the
        // question was asked, which was three characters ago.
        item.replace = Some((14, 14));
        v.lsp.popup = Some(Popup {
            items: vec![item],
            // Asked just after the dot; the cursor is now at 17.
            origin: (0, 14),
            typed: "pus".to_string(),
            ..Popup::default()
        });
        v.accept_completion();
        assert_eq!(
            v.open.as_ref().unwrap().lines[0],
            "fn main() { c.push",
            "the three letters already typed must be replaced, not kept"
        );
        assert_eq!(v.open.as_ref().unwrap().col, 18);
    }

    /// **Both spellings of the completion key reach the page, and the second
    /// one did not for a fortnight.**
    ///
    /// `takes_key` admitted exactly one `Ctrl` chord, so the `Ctrl+space` arm
    /// below it was unreachable and the key did nothing at all -- while the
    /// help row, the module header and a commit message all named it. Relaxing
    /// the predicate alone would have been worse: the blanket `Ctrl` arm in
    /// `handle_key` would have *saved the file* when somebody asked for a
    /// completion. So this asserts both halves.
    #[test]
    fn ctrl_space_asks_for_a_completion_and_does_not_save() {
        let mut v = with_word("pu");
        let chord = KeyEvent::new(KeyCode::Char(' '), KeyModifiers::CONTROL);
        assert!(takes_key(&v, chord), "the chord never reached the page");
        assert_eq!(
            handle_key(&mut v, chord),
            CodeAction::Complete,
            "Ctrl+space must ask, not save"
        );
        // And the one chord that does save still does. The buffer has to be
        // dirty for a save to be a write rather than a sentence.
        v.open.as_mut().expect("open").dirty = true;
        let save = KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL);
        assert!(matches!(handle_key(&mut v, save), CodeAction::Save(_)));
    }

    /// **Completion refuses outside the editor, because its popup has no exit
    /// there.** `popup_key` is reached only from `edit_key`, so a list opened
    /// from the tree or from a read-only document covered the code with
    /// nothing able to dismiss it, and `Esc` closed the whole page instead.
    #[test]
    fn a_completion_asked_for_outside_the_editor_refuses_in_words() {
        let mut v = sample();
        open_file(&mut v, "src/main.rs", &["fn main() {}"]);
        v.body_rows = 10;
        assert!(!v.editing(), "the fixture must be a viewer, not an editor");
        assert_eq!(
            handle_key(&mut v, key(KeyCode::F(9))),
            CodeAction::FocusChanged
        );
        assert!(
            v.lsp.popup.is_none(),
            "a popup opened with no way to close it"
        );
        let note = lsp_line(&v).expect("a sentence");
        assert!(note.contains("press Enter to edit"), "{note}");

        // And once the document is being edited it asks, as it always did.
        assert_eq!(
            handle_key(&mut v, key(KeyCode::Enter)),
            CodeAction::FocusChanged
        );
        assert!(v.editing());
        assert_eq!(handle_key(&mut v, key(KeyCode::F(9))), CodeAction::Complete);
    }

    /// **Opening a file drops every answer about the last one.** The popup is
    /// the one that could do damage rather than merely mislead: it inserts into
    /// the buffer, so a list left standing across a file switch would put the
    /// old file's candidate into the new file's text.
    #[test]
    fn opening_a_file_drops_the_answers_about_the_previous_one() {
        use emma_tools_lsp::render::{Token, TokenKind};
        let mut v = with_word("pu");
        v.lsp.popup = Some(Popup {
            items: vec![candidate("push", "push", "push")],
            ..Popup::default()
        });
        v.lsp.places = Some(Places {
            about: "references to widget".into(),
            rows: vec![Place {
                label: "src/lib.rs:1".into(),
                rel: "src/lib.rs".into(),
                line: 0,
                col: 0,
            }],
            at: 0,
        });
        v.lsp.hover = Some(HoverPopup {
            lines: vec!["fn push".into()],
            scroll: 0,
        });
        v.lsp.tokens = vec![Token {
            line: 0,
            start: 0,
            end: 2,
            kind: TokenKind::Keyword,
        }];
        v.lsp.tokens_for = Some("src/lib.rs".into());

        open_file(&mut v, "src/other.rs", &["fn other() {}"]);

        assert!(
            v.lsp.popup.is_none(),
            "a completion for the old file survived"
        );
        assert!(
            v.lsp.places.is_none(),
            "a places panel about the old file survived"
        );
        assert!(v.lsp.hover.is_none(), "a hover about the old file survived");
        assert!(v.lsp.tokens.is_empty(), "the old file's colours survived");
        assert_eq!(v.lsp.tokens_for, None);
    }

    /// **The popup owns six keys and no more.** One that swallowed Backspace
    /// would strand somebody mid-word; one that swallowed a letter would stop
    /// them typing. Both must reach the editor.
    #[test]
    fn the_popup_takes_its_own_keys_and_lets_the_editor_keep_the_rest() {
        let mut v = with_word("pu");
        v.lsp.popup = Some(Popup {
            items: vec![
                candidate("push(…)", "push", "push"),
                candidate("pull(…)", "pull", "pull"),
            ],
            typed: "pu".to_string(),
            ..Popup::default()
        });
        // Down moves the selection and does not reach the buffer.
        let before = v.open.as_ref().unwrap().lines[0].clone();
        handle_key(&mut v, key(KeyCode::Down));
        assert_eq!(v.lsp.popup.as_ref().unwrap().at, 1);
        assert_eq!(
            v.open.as_ref().unwrap().lines[0],
            before,
            "Down typed a character"
        );

        // A letter reaches the buffer *and* narrows the list.
        handle_key(&mut v, key(KeyCode::Char('s')));
        assert_eq!(v.open.as_ref().unwrap().lines[0], "fn main() { pus");
        assert_eq!(
            v.lsp.popup.as_ref().map(|p| p.typed.as_str()),
            Some("pus"),
            "typing did not narrow the open popup"
        );
        assert_eq!(
            v.lsp.popup.as_ref().unwrap().visible().len(),
            1,
            "the list did not narrow"
        );

        // Backspace reaches the buffer, which is the key that proves the popup
        // is a layer and not a modal.
        handle_key(&mut v, key(KeyCode::Backspace));
        assert_eq!(v.open.as_ref().unwrap().lines[0], "fn main() { pu");

        // Esc closes it and leaves the buffer alone.
        handle_key(&mut v, key(KeyCode::Esc));
        assert!(v.lsp.popup.is_none());
        assert_eq!(v.open.as_ref().unwrap().lines[0], "fn main() { pu");
    }

    /// Typing past every match closes the popup rather than leaving a list of
    /// items that do not contain what is on screen.
    #[test]
    fn typing_past_every_match_closes_the_popup() {
        let mut v = with_word("pu");
        v.lsp.popup = Some(Popup {
            items: vec![candidate("push(…)", "push", "push")],
            typed: "pu".to_string(),
            ..Popup::default()
        });
        handle_key(&mut v, key(KeyCode::Char('z')));
        assert!(
            v.lsp.popup.is_none(),
            "a popup with nothing matching stayed open"
        );
        assert_eq!(v.open.as_ref().unwrap().lines[0], "fn main() { puz");
    }

    /// **Moving the selection changes what is inserted**, which sounds obvious
    /// and was untested: every other case here chooses row zero, so a `chosen`
    /// that always returned the first item passed the whole file. Found by
    /// mutating it and watching nothing go red.
    #[test]
    fn the_selected_row_is_the_one_that_gets_inserted() {
        let mut v = with_word("p");
        v.lsp.popup = Some(Popup {
            items: vec![
                candidate("push(…)", "push", "push"),
                candidate("pop(…)", "pop", "pop"),
            ],
            typed: "p".to_string(),
            ..Popup::default()
        });
        handle_key(&mut v, key(KeyCode::Down));
        handle_key(&mut v, key(KeyCode::Enter));
        assert_eq!(
            v.open.as_ref().unwrap().lines[0],
            "fn main() { pop",
            "Enter inserted the first row rather than the selected one"
        );
    }

    /// Accepting when nothing matches inserts nothing. Inserting the first item
    /// of a list the person has typed past is how an editor writes something
    /// nobody asked for.
    #[test]
    fn accepting_with_no_match_inserts_nothing() {
        let mut v = with_word("pu");
        v.lsp.popup = Some(Popup {
            items: vec![candidate("push(…)", "push", "push")],
            typed: "zzz".to_string(),
            ..Popup::default()
        });
        v.accept_completion();
        assert_eq!(v.open.as_ref().unwrap().lines[0], "fn main() { pu");
        assert!(v.lsp.popup.is_none());
    }

    // -- opening by itself ---------------------------------------------------

    /// A view that has been told what the server's trigger characters are, the
    /// way the bridge tells it after a handshake.
    fn with_triggers(word: &str) -> CodeView {
        let mut v = with_word(word);
        v.apply_lsp(LspUpdate::Triggers {
            completion: vec![".".to_string(), ":".to_string()],
            signature: vec!["(".to_string()],
        });
        v
    }

    /// **A trigger character opens a list immediately**, with nothing typed,
    /// because after a dot the useful answer is the whole set of members.
    #[test]
    fn a_trigger_character_asks_at_once() {
        let mut v = with_triggers("c");
        handle_key(&mut v, key(KeyCode::Char('.')));
        assert!(
            v.lsp.want_completion,
            "a dot did not ask, so the list never opens by itself"
        );
    }

    /// An identifier opens one only once there is enough to narrow it. A list
    /// of everything in scope after one letter is a list nobody reads.
    #[test]
    fn an_identifier_asks_only_once_it_is_long_enough_to_narrow() {
        let mut v = with_triggers("");
        handle_key(&mut v, key(KeyCode::Char('p')));
        assert!(!v.lsp.want_completion, "one letter is not enough");
        handle_key(&mut v, key(KeyCode::Char('u')));
        assert!(!v.lsp.want_completion, "two letters is not enough");
        handle_key(&mut v, key(KeyCode::Char('s')));
        assert!(
            v.lsp.want_completion,
            "the list must open on the third letter, where every editor puts it"
        );
    }

    /// Nothing asks for a character that starts no word and triggers nothing:
    /// a space, a bracket the server did not name.
    #[test]
    fn an_ordinary_character_asks_for_nothing() {
        let mut v = with_triggers("push");
        handle_key(&mut v, key(KeyCode::Char(' ')));
        assert!(!v.lsp.want_completion);
        handle_key(&mut v, key(KeyCode::Char(';')));
        assert!(!v.lsp.want_completion);
    }

    /// **Typing into an open popup narrows it rather than asking again**, which
    /// is what keeps this from being one request per keystroke. The exception
    /// is a list the server truncated and asked to be re-queried.
    #[test]
    fn typing_into_an_open_list_narrows_it_unless_the_server_wants_re_asking() {
        let mut v = with_triggers("pu");
        v.lsp.popup = Some(Popup {
            items: vec![candidate("push(…)", "push", "push")],
            typed: "pu".to_string(),
            ..Popup::default()
        });
        handle_key(&mut v, key(KeyCode::Char('s')));
        assert!(
            !v.lsp.want_completion,
            "an open list was re-asked instead of narrowed"
        );

        // The same keystroke against a list the server marked incomplete.
        let mut v = with_triggers("pu");
        v.lsp.popup = Some(Popup {
            items: vec![candidate("push(…)", "push", "push")],
            typed: "pu".to_string(),
            incomplete: true,
            ..Popup::default()
        });
        handle_key(&mut v, key(KeyCode::Char('s')));
        assert!(
            v.lsp.want_completion,
            "a truncated list must be re-asked, or it only ever narrows"
        );
    }

    /// A server that names no trigger characters gets no guesses. Emma asks
    /// where the language says to ask, and nowhere else.
    #[test]
    fn with_no_triggers_named_a_dot_asks_for_nothing() {
        let mut v = with_word("c");
        handle_key(&mut v, key(KeyCode::Char('.')));
        assert!(
            !v.lsp.want_completion,
            "a dot was hard-coded rather than read from the server"
        );
    }

    // -- syntax colour -------------------------------------------------------

    /// Tokens about another file are not painted over this one. A stale set is
    /// dropped rather than drawn, which is the same rule the diagnostics follow.
    #[test]
    fn tokens_for_another_file_are_not_painted_over_this_one() {
        use emma_tools_lsp::render::{Token, TokenKind};
        let mut v = sample();
        v.body_rows = 10;
        open_file(&mut v, "src/lib.rs", &["let x = 1; // why"]);
        v.apply_lsp(LspUpdate::Tokens {
            path: "src/other.rs".to_string(),
            items: vec![Token {
                line: 0,
                start: 0,
                end: 3,
                kind: TokenKind::Comment,
            }],
        });
        assert!(
            v.lsp.tokens.is_empty(),
            "a token set about a different file was kept"
        );

        // **And the painter refuses one too**, which `apply_lsp` above makes
        // unreachable through the normal path. Set directly, because a guard
        // nothing can reach is a guard nothing tests: mutating the painter's
        // check passed the whole file until this half existed.
        v.lsp.tokens_for = Some("src/other.rs".to_string());
        v.lsp.tokens = vec![Token {
            line: 0,
            start: 0,
            end: 3,
            kind: TokenKind::Comment,
        }];
        let area = Rect::new(0, 0, 80, 12);
        let mut buf = Buffer::empty(area);
        render(area, &mut buf, &v, &skin());
        let g = doc_geom(area, &v).expect("drawn");
        let first = buf[(g.area.x + g.gutter, g.area.y)].style().fg;
        let plain = buf[(g.area.x + g.gutter + 4, g.area.y)].style().fg;
        assert_eq!(
            first, plain,
            "tokens belonging to another file were painted over this one"
        );
    }
}
