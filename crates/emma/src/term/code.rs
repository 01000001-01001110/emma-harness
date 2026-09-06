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
//! # What this stage does not do
//!
//! The page arrives in three stages, and this is the second. There is no chat
//! strip (stage c) and no language-server decoration (the LSP half) here;
//! both are named where their seam will go, so the next stage extends this
//! file rather than reinterpreting it.

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
/// Tree and Body; History has Tree, Commits and Diff.
///
/// Stage c adds `Chat` for the strip under the file; [`CodeView::cycle_focus`]
/// is where it joins the cycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Focus {
    #[default]
    Tree,
    Body,
    Commits,
    Diff,
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

// region: Keys
// ---------------------------------------------------------------------------
// Keys — the pure seam
// ---------------------------------------------------------------------------

/// What a key asks the shell to do. Everything that needs a file, git or a
/// process crosses this enum; the handler below never touches disk, which is
/// what makes it testable without a repository.
///
/// Stage c adds `Ask(String)`; the LSP half adds `Hover` and `Definition`.
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
pub fn takes_key(v: &CodeView, key: KeyEvent) -> bool {
    if key.kind == KeyEventKind::Release || key.modifiers.contains(KeyModifiers::ALT) {
        return false;
    }
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        return key.code == KeyCode::Char('s') && v.editing();
    }
    true
}

/// One key, against the whole page.
///
/// Five layers, in this order, because each can only be reached by getting
/// past the one above it:
///
/// 1. **[`takes_key`]** — releases and Alt are never ours, and neither is Ctrl
///    but for the save chord.
/// 2. **The save chord**, above the warning below it, because saving is the
///    way *out* of an unsaved-changes warning and must not be the key that
///    cancels one.
/// 3. **An armed warning**, which consumes the key that answers it.
/// 4. **The function keys**, which work from anywhere on the page — including
///    from inside the document, where every letter is text. That is the whole
///    reason they are function keys.
/// 5. The editor, when the document is being edited, or the browser otherwise.
pub fn handle_key(v: &mut CodeView, key: KeyEvent) -> CodeAction {
    if !takes_key(v, key) {
        return CodeAction::None;
    }
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        // `takes_key` let exactly one chord through.
        return v.request_save();
    }
    if key.code == KeyCode::F(2) {
        return v.request_save();
    }
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
    match key.code {
        KeyCode::F(3) => return v.toggle_mode(),
        KeyCode::F(4) => return v.copy(),
        // The second door (owner ruling D1, 2026-09-06). A function key
        // because it has to keep working with the document taking letters,
        // and because the header names it beside the `[Editor]` button that
        // does the same thing.
        KeyCode::F(7) => return CodeAction::LaunchEditor,
        _ => {}
    }
    v.notice = None;
    if v.editing() {
        return edit_key(v, key);
    }
    browse_key(v, key)
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

/// The document's keys. Every printable character is text, which is why the
/// browser's `j`/`k`/`g` shortcuts are not reachable from here: `Tab` cycles
/// the panes and that is where they live, and `Esc` puts the document back to
/// being read.
///
/// Shift with an arrow extends a selection; an arrow without it drops one.
/// Typing, Enter, Backspace and Delete all replace a live selection, which is
/// what every editor does and what makes a selection worth having.
fn edit_key(v: &mut CodeView, key: KeyEvent) -> CodeAction {
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
        }
        _ => return CodeAction::None,
    }
    o.follow_cursor(rows);
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

    /// Tab, round the regions the current mode has. Stage c inserts the chat
    /// strip after the body and after the patch.
    fn cycle_focus(&mut self) {
        self.focus = match (self.mode, self.focus) {
            (Mode::File, Focus::Tree) => Focus::Body,
            (Mode::File, _) => Focus::Tree,
            (Mode::History, Focus::Tree) => Focus::Commits,
            (Mode::History, Focus::Commits) => Focus::Diff,
            (Mode::History, _) => Focus::Tree,
        };
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
    let help_h = help_rows(inner.height, y);
    let h = inner.height.saturating_sub(y + help_h);
    Rect::new(inner.x, inner.y + y, inner.width, h)
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
    cell_to_pos(v, area, col, row).map(|(l, c)| CodeClick::Doc(l, c))
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
    let help_h = help_rows(inner.height, y);
    let content_h = content.height;
    if content.height > 0 {
        match v.mode {
            Mode::File => draw_file(content, buf, v, skin),
            Mode::History => draw_history(content, buf, v, skin),
        }
        out.rows = content.height as usize;
    }
    if help_h == 1 {
        put(
            buf,
            inner,
            y + content_h,
            Line::from(Span::styled(
                fit(&help_line(v), inner.width as usize, skin.glyphs.ellipsis),
                skin.palette.dim(),
            )),
        );
    }
    out
}

/// The page's keys, named where a reader will look for them. Four rows and not
/// one, because the editor's keys and the browser's mean different things on
/// the same keyboard and a row naming both would be a row naming neither.
fn help_line(v: &CodeView) -> String {
    if v.editing() {
        return "typing edits · Ctrl+s or F2 saves · Shift+arrows select · F4 copies \
                · Esc read-only"
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
        Mode::File => "Tab pane · ↑/↓ move · Enter opens, then edits · F3 HISTORY · F4 copy \
                       · F7 external editor"
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
            Line::from(dim(
                skin,
                "select a file: ↑/↓ move, Enter opens, F3 for history",
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
    for (i, ln) in open.lines.iter().enumerate().skip(top).take(h) {
        let row_chars: Vec<char> = ln.chars().collect();
        let (sel_a, sel_b) = match sel {
            Some((a, b)) if i >= a.0 && i <= b.0 => (
                if i == a.0 { a.1 } else { 0 },
                if i == b.0 { b.1 } else { row_chars.len() },
            ),
            _ => (0, 0),
        };
        let cursor = (editing && i == open.line).then_some(open.col);
        let mut spans = vec![Span::styled(
            format!("{:>gw$} ", i + 1, gw = gw),
            skin.palette.dim(),
        )];
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
            let style = if cursor == Some(idx) {
                skin.palette.chip(Role::Accent)
            } else if selected {
                skin.palette.chip(Role::Info)
            } else {
                skin.palette.style(Role::Text)
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
        assert!(r.rows >= 18, "the body reported {} rows", r.rows);
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
}
