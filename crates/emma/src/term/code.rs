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
//! writes past its pane's inner width.
//!
//! # What this stage does not do
//!
//! The page arrives in three stages, and this is the first. There is **no
//! editor, no save path, no clipboard, no chat strip and no language-server
//! decoration** here — every one of those is named where its seam will go, so
//! the next stage extends this file rather than reinterpreting it. The viewer
//! is read-only, which is why [`OpenFile`] has no cursor: a cursor nothing can
//! type at is a promise the page cannot keep.

use std::collections::BTreeSet;
use std::path::PathBuf;

use ratatui::buffer::Buffer;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Widget};

use super::code_git::{Commit, DiffKind, DiffRow, FileRead};
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

/// The open file: its lines, or the honest refusal in `note` (binary, too
/// large, not UTF-8) with no lines drawn.
///
/// Stage b makes this the editor's document and gives it a `(line, col)`
/// cursor, a dirty flag and the hash the save path compares against. Here it
/// is what the viewer needs and no more.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenFile {
    pub path: String,
    /// Sanitised at read time — see the module header.
    pub lines: Vec<String>,
    pub note: Option<String>,
    pub scroll: usize,
    /// Whether sanitising had to change anything. The header says so, and
    /// stage b's save path refuses a buffer that carries it.
    pub sanitised: bool,
}

impl OpenFile {
    /// A file the shell has just read, made safe to draw.
    ///
    /// The sanitising happens **here** rather than in `code_git::read_file`
    /// for one reason: `read_file` is also what a save path compares bytes
    /// against, and a reader that quietly rewrites what it returns would make
    /// the round trip lie. The page is the consumer that needs cells, so the
    /// page is where the bytes become cells.
    pub fn from_read(path: String, read: FileRead) -> Self {
        match read {
            FileRead::Text(text) => {
                let lines: Vec<String> = text.lines.iter().map(|l| sanitise(l)).collect();
                let sanitised = lines.iter().zip(&text.lines).any(|(a, b)| a != b);
                Self {
                    path,
                    lines,
                    note: None,
                    scroll: 0,
                    sanitised,
                }
            }
            FileRead::Refused(why) => Self {
                path,
                lines: Vec::new(),
                note: Some(why),
                scroll: 0,
                sanitised: false,
            },
        }
    }

    /// Whether there is text to draw. A refusal note means there is not, and
    /// the pane says the note instead of an empty region.
    pub fn has_text(&self) -> bool {
        self.note.is_none()
    }
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
    /// One line under the header: a refusal, or what the page just declined
    /// to do. Cleared by the next key that acts.
    pub notice: Option<String>,
    /// How many document rows the last paint had room for. Only the paint
    /// knows it, so it is stored here for PageUp/PageDown.
    pub body_rows: usize,
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
            body_rows: 0,
        }
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
    pub fn set_open(&mut self, path: String, read: FileRead) {
        self.open = Some(OpenFile::from_read(path, read));
        self.history = None;
        self.mode = Mode::File;
        self.focus = Focus::Body;
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
/// Stage b adds `Save` and `Copy(String)`; stage c adds `Ask(String)`; the
/// LSP half adds `Hover` and `Definition`.
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
pub fn takes_key(key: KeyEvent) -> bool {
    key.kind != KeyEventKind::Release
        && !key
            .modifiers
            .intersects(KeyModifiers::ALT | KeyModifiers::CONTROL)
}

/// One key, against the whole page.
///
/// Three layers, in this order, because each can only be reached by getting
/// past the one above it:
///
/// 1. **[`takes_key`]** — releases, Alt and Ctrl are never ours.
/// 2. **The function keys**, which work from anywhere on the page — including
///    from inside the document, where stage b will make every letter text.
///    That is the whole reason they are function keys.
/// 3. The browser: the tree, the commit list, the patch.
pub fn handle_key(v: &mut CodeView, key: KeyEvent) -> CodeAction {
    if !takes_key(key) {
        return CodeAction::None;
    }
    match key.code {
        KeyCode::F(3) => return v.toggle_mode(),
        // The second door (owner ruling D1, 2026-09-06). A function key
        // because it has to keep working once stage b makes the document take
        // letters, and because the header names it beside the `[Editor]`
        // button that does the same thing.
        KeyCode::F(7) => return CodeAction::LaunchEditor,
        _ => {}
    }
    v.notice = None;
    browse_key(v, key)
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
            Mode::File => CodeAction::Close,
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
            CodeAction::Open(node.path)
        }
    }

    fn page(&mut self, up: bool) -> CodeAction {
        let rows = self.body_rows.max(1) as i32;
        self.move_by(if up { -rows } else { rows })
    }
}

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

/// The header's one button. Stage b adds `[Save]` and `[Copy]` to its left,
/// which is why [`header_bar`] walks leftwards from the tabs.
const EDITOR_LABEL: &str = "[Editor]";

/// Every control in the header row, laid out once so the paint and the hit
/// test cannot disagree about where anything is. Right-aligned, and each
/// control appears only when the pane is wide enough to hold it and
/// everything to its right.
#[derive(Debug, Clone, Copy, Default)]
struct HeaderBar {
    file_tab: Option<Rect>,
    history_tab: Option<Rect>,
    editor: Option<Rect>,
}

fn header_bar(inner: Rect) -> HeaderBar {
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
    let w = EDITOR_LABEL.len() as u16;
    if inner.width >= MODE_W + w + 2 {
        bar.editor = Some(Rect::new(mode_x - w - 1, inner.y, w, 1));
    }
    bar
}

/// How many rows the help line under the content takes: one, when there is
/// enough body left that spending a row on it is not stealing the file's.
fn help_rows(inner_height: u16, y: u16) -> u16 {
    u16::from(inner_height.saturating_sub(y) >= 3)
}

/// A click on the page, or nothing.
///
/// Stage b adds `Save`, `CopyFile` and `Doc(line, col)`; `Doc` is what needs
/// the geometry the paint recorded, which is why [`click`] already takes the
/// view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodeClick {
    /// Switch to this tab.
    Tab(Mode),
    /// Hand the repository to the external editor.
    Editor,
}

/// Hit-test a press against the page's controls, using the same geometry the
/// paint used.
///
/// `None` for every other cell, deliberately: a page where two controls work
/// must not swallow the rest of the surface.
pub fn click(_v: &CodeView, area: Rect, col: u16, row: u16) -> Option<CodeClick> {
    let r = split(area);
    let inner = Block::bordered().inner(r.body);
    let bar = header_bar(inner);
    let on = |rect: Option<Rect>| rect.is_some_and(|q| row == q.y && col >= q.x && col < q.right());
    if on(bar.editor) {
        return Some(CodeClick::Editor);
    }
    if on(bar.file_tab) {
        return Some(CodeClick::Tab(Mode::File));
    }
    if on(bar.history_tab) {
        return Some(CodeClick::Tab(Mode::History));
    }
    None
}

/// What a [`CodeClick`] asks the shell to do, so the click and the key it
/// duplicates cannot drift apart: one dispatch, two ways in.
pub fn act(v: &mut CodeView, hit: CodeClick) -> CodeAction {
    v.notice = None;
    match hit {
        CodeClick::Editor => CodeAction::LaunchEditor,
        CodeClick::Tab(Mode::File) if v.mode == Mode::History => v.toggle_mode(),
        CodeClick::Tab(Mode::History) if v.mode == Mode::File => v.toggle_mode(),
        CodeClick::Tab(_) => CodeAction::FocusChanged,
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

    let bar = header_bar(inner);
    out.file_tab = bar.file_tab;
    out.history_tab = bar.history_tab;
    out.editor = bar.editor;
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
            Some(o) if o.sanitised => format!("{}  ({SANITISED_NOTE})", sanitise(&o.path)),
            Some(o) => sanitise(&o.path),
            None => "no file selected".to_string(),
        }
    };
    let leftmost = bar.editor.or(bar.file_tab).map_or(inner.right(), |r| r.x);
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

    let help_h = help_rows(inner.height, y);
    let content_h = inner.height.saturating_sub(y + help_h);
    let content = Rect::new(inner.x, inner.y + y, inner.width, content_h);
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

/// The page's keys, named where a reader will look for them. Honest about
/// what this stage does not do: the viewer is read-only, and the row says so
/// rather than letting somebody discover it by typing.
fn help_line(v: &CodeView) -> String {
    match v.mode {
        Mode::History if v.history.as_ref().is_some_and(|h| h.showing_diff) => {
            "↑/↓ scroll the patch · b or Esc back to the commits · F3 or click FILE".to_string()
        }
        Mode::History => {
            "↑/↓ pick a commit · Enter shows its patch · b back to the file · F3 or click FILE"
                .to_string()
        }
        Mode::File => "Tab pane · ↑/↓ move · Enter opens · F3 HISTORY · F7 external editor \
                       · read-only here"
            .to_string(),
    }
}

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
    let h = area.height as usize;
    let total = open.lines.len();
    let top = open.scroll.min(total.saturating_sub(h));
    let gw = digits(total.max(1)).max(2);
    if area.width as usize <= gw + 1 {
        return;
    }
    let text_w = area.width as usize - gw - 1;
    for (i, ln) in open.lines.iter().enumerate().skip(top).take(h) {
        let line = Line::from(vec![
            Span::styled(format!("{:>gw$} ", i + 1, gw = gw), skin.palette.dim()),
            // Already sanitised at read time; `fit` is the column budget.
            Span::styled(
                fit(ln, text_w, skin.glyphs.ellipsis),
                skin.palette.style(Role::Text),
            ),
        ]);
        put(buf, area, (i - top) as u16, line);
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
        let o = OpenFile::from_read(
            "src/main.rs".to_string(),
            text(&["let x = 1;\u{1b}[31m", "\tindented"]),
        );
        assert!(
            !o.lines.iter().any(|l| l.contains('\u{1b}')),
            "an ESC survived the read: {:?}",
            o.lines
        );
        assert_eq!(o.lines[1], "    indented", "a TAB is not one cell");
        assert!(o.sanitised, "the page must know the bytes were changed");
    }

    #[test]
    fn a_sanitised_file_says_so_in_the_header_so_a_save_can_refuse_it() {
        let mut v = sample();
        v.set_open("src/main.rs".to_string(), text(&["\tx"]));
        let (buf, _) = painted(&v, Rect::new(0, 0, 100, 12));
        let head = dump(&buf).join("\n");
        assert!(head.contains(SANITISED_NOTE), "{head}");
    }

    #[test]
    fn a_clean_file_is_not_marked_as_changed() {
        let o = OpenFile::from_read("a.rs".to_string(), text(&["fn main() {}"]));
        assert!(!o.sanitised);
    }

    #[test]
    fn a_commit_subject_and_a_patch_row_lose_their_escape_bytes_too() {
        let mut v = sample();
        v.set_open("src/main.rs".to_string(), text(&["x"]));
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
        v.set_open("src/main.rs".to_string(), text(&["x"]));
        assert_eq!(
            handle_key(&mut v, key(KeyCode::F(3))),
            CodeAction::LoadHistory
        );
        assert_eq!(v.mode, Mode::History);
    }

    #[test]
    fn selecting_the_next_commit_asks_for_its_patch_and_drops_the_old_one() {
        let mut v = sample();
        v.set_open("src/main.rs".to_string(), text(&["x"]));
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
        v.set_open("src/main.rs".to_string(), text(&["x"]));
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
        v.set_open("src/main.rs".to_string(), text(&["x"]));
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
        v.set_open("src/main.rs".to_string(), text(&["x"]));
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
        v.set_open("src/main.rs".to_string(), text(&["x"]));
        v.mode = Mode::History;
        assert_eq!(v.set_history(Vec::new(), None), None);
        let (buf, _) = painted(&v, Rect::new(0, 0, 100, 12));
        assert!(dump(&buf).join("\n").contains("no history for this file"));
    }

    #[test]
    fn an_unanswered_patch_says_loading_and_never_changed_nothing() {
        let mut v = sample();
        v.set_open("src/main.rs".to_string(), text(&["x"]));
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
        v.set_open("src/main.rs".to_string(), text(&["fn main() {", "}"]));
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
        );
        let (buf, _) = painted(&v, Rect::new(0, 0, 100, 12));
        assert!(dump(&buf).join("\n").contains("binary file"));
    }

    #[test]
    fn a_long_file_paints_every_row_of_the_pane_and_the_paint_reports_them() {
        let lines: Vec<String> = (0..200).map(|i| format!("line {i}")).collect();
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        let mut v = sample();
        v.set_open("big.rs".to_string(), text(&refs));
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
        v.set_open("big.rs".to_string(), text(&refs));
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
        v.set_open("big.rs".to_string(), text(&refs));
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
        v.set_open("src/main.rs".to_string(), text(&["x"]));
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
        open.set_open("src/main.rs".to_string(), text(&["x"]));
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
            let acted = handle_key(&mut v, k) != CodeAction::None;
            assert_eq!(takes_key(k), acted, "{m:?} disagrees");
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
        v.set_open("a.rs".to_string(), text(&["x"]));
        v.set_history(vec![commit("aaaaaaa", "one")], None);
        v.set_open("b.rs".to_string(), text(&["y"]));
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
}
