//! The Memory main page: the owner's mock, cell for cell.
//!
//! The mock is the acceptance criterion (`notes/design-memory-page.md`), the
//! same standing rule the Settings screen was built under: action bar, four
//! cards in a two-column grid, a full-width categories card, a tip row, and a
//! bordered query box, with every label, glyph and affordance from the mock.
//!
//! The one sanctioned divergence is data. Emma's memory is a per-repo LLM
//! wiki still being built (plan M1, rev 2), and rendering the mock's stored
//! memories as if they existed would be false chrome — the status bar law,
//! page-sized. So everything renders from a [`MemoryView`]: a populated view
//! reproduces the mock exactly (the tests pin that now, with the mock's
//! sample data), and an empty view keeps the exact card chrome with an honest
//! one-line dim empty state and real zeros. Real data lights the page up
//! later with zero further UI work.
//!
//! Interaction (plan M5) is two pure halves. [`Focus`] names one focusable
//! element and the renderer paints it: the owning card's border takes the
//! accent, a focused row wears the sidebar's chip band, a focused affordance
//! wears the chip on just itself. [`handle_key`] maps one key to one
//! [`MemoryAction`] and never touches a file; the shell
//! (`App::memory_key`) applies actions through `crate::memory` and rebuilds
//! the view, so the page always shows disk truth. Two key scopes, the
//! owner's rule: query box focused, printable keys type; otherwise the
//! action bar acts. [`PageMode`] adds the two derived sub-views (ALL
//! MEMORIES, MANAGE PINNED) behind the header affordances.
//!
//! Pure rendering, like [`super::settings`]: the shell owns whether the page
//! is open, and mounting is one line in the layout seam once feat/ui-split
//! lands (`m` / Alt+m select it). All width arithmetic is in display columns
//! via [`cols`]/[`fit`], and no row ever writes past its card's inner width:
//! the trailing column survives and the text truncates, the sidebar's ruling.
//! The tip row and query box are bottom-anchored like the chat input box, and
//! cards compress honestly (`… N more`) when the window runs short.

use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Widget};

use super::palette::Role;
use super::render::{cols, corner_row, fit, Skin, ASCII};

// region: State
// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

/// Where a recent memory came from: a document-shaped source or a chat.
/// Decides the row's leading glyph, nothing else.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Doc,
    Chat,
}

/// One row of the RECENT MEMORIES card.
#[derive(Debug, Clone)]
pub struct Recent {
    pub source: Source,
    pub text: String,
    /// Clock text, right-aligned dim — `10:15 AM` shapes, caller-formatted.
    pub time: String,
    /// The store slug behind the row — what [`MemoryAction::Pin`] and friends
    /// carry. Empty only in a view no store backs.
    pub slug: String,
}

/// One listed memory, for the pinned card and the two sub-views. The
/// category is an index into [`CATEGORIES`], so this layer needs no store
/// types; the shell maps it to `crate::memory::Category` (same order).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemRow {
    pub slug: String,
    pub title: String,
    pub category: usize,
    pub created: String,
    pub pinned: bool,
}

/// Which page the memory screen shows. `Main` is the mock; the two sub-views
/// are derived designs behind its header affordances (owner directive,
/// 2026-08-26): same head idiom, same card chrome, same key scopes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PageMode {
    #[default]
    Main,
    AllMemories,
    ManagePinned,
}

/// Who spoke a transcript line in the CONVERSATION MEMORY card.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Speaker {
    Emma,
    You,
}

/// The retrieval index, present only once the wiki's index stage exists.
/// `None` renders the full RETRIEVAL STATUS structure with [`NO_INDEX`] as
/// the status value — the structure is chrome, the values are claims.
#[derive(Debug, Clone)]
pub struct IndexView {
    pub healthy: bool,
    pub last_updated: String,
    pub indexed_tokens: String,
    pub top_score_avg: String,
    pub latency: String,
    /// Index coverage, whole percent, drives the gauge row.
    pub coverage_pct: u8,
}

/// One focusable element on the page. Render-only today: the shell will set
/// it from keys in M5; this layer just paints it. The owning card's border
/// takes the accent; a row target wears the full-width chip band; an
/// affordance target wears the chip on just itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    /// A row of RECENT MEMORIES, by index.
    RecentRow(usize),
    /// The RECENT MEMORIES header's `View all →`.
    RecentViewAll,
    /// The trailing `···` of a PINNED FACTS row, by index.
    PinnedDots(usize),
    /// The PINNED FACTS header's `Manage →`.
    PinnedManage,
    /// The CONVERSATION MEMORY footer's `View full history →`.
    ConvoHistory,
    /// The RETRIEVAL STATUS card as a whole (its rows are read-only).
    Retrieval,
    /// A MEMORY CATEGORIES mini-box, by index into [`CATEGORIES`].
    CategoryBox(usize),
    /// The query box.
    QueryBox,
}

/// What the page shows. Today every field defaults empty/zero because no
/// store exists; each backend stage (M1..M5) fills more of it.
#[derive(Debug, Clone, Default)]
pub struct MemoryView {
    /// The running binary's version, `v`-prefixed — the mock's top-right corner.
    pub version: String,
    pub recent: Vec<Recent>,
    pub pinned: Vec<MemRow>,
    /// Display form of the conversation token estimate (`~2.1k`). Empty
    /// renders as `0` — a real zero, not a sample.
    pub convo_tokens: String,
    pub convo_messages: u64,
    pub transcript: Vec<(Speaker, String)>,
    /// Whether older messages were really archived; the footer's claim only
    /// renders when it is true.
    pub older_archived: bool,
    pub index: Option<IndexView>,
    pub total_memories: u64,
    /// The `Embedding model` row's value, rendered verbatim whether or not an
    /// index exists — the live value is `index-first (none)` until an
    /// embedding stage does (plan rev 2: memory is a wiki, not a vector store).
    pub embedding_model: String,
    /// Counts per category, in [`CATEGORIES`] order.
    pub category_counts: [u64; 6],
    /// The tip row's right half claims auto-save; only claim it when true.
    pub auto_save: bool,
    /// The query box's current text; empty shows the placeholder.
    pub query: String,
    /// The focused element, if any. `None` is the mock's own state.
    pub focus: Option<Focus>,
    /// A one-line notice, shown in the tip row's place until dismissed.
    /// Honesty channel: "search needs M2" lives here, not in silence.
    pub notice: Option<String>,
    /// Which page is showing: the mock, or one of its two sub-views.
    pub mode: PageMode,
    /// Every live memory, for the ALL MEMORIES sub-view.
    pub all: Vec<MemRow>,
    /// ALL MEMORIES: selection index into the *filtered* listing.
    pub all_selected: usize,
    /// ALL MEMORIES: the category filter, an index into [`CATEGORIES`].
    /// `None` shows everything.
    pub all_filter: Option<usize>,
    /// MANAGE PINNED: selection index into [`Self::pinned`].
    pub pin_selected: usize,
    /// The add flow: `Some(category index)` while `[a]` has the query box.
    pub adding: Option<usize>,
}

/// The subtitle under the title, verbatim from the mock.
pub const SUBTITLE: &str = "Assistant memory workspace";

/// The key that leaves this page, drawn on it.
///
/// **`Alt+m`, not `Esc`.** Esc on this page clears the focus and the add
/// flow's half-typed text ([`handle_key`]); it does not close anything. The
/// chord that opened the page is the chord that closes it, and a page naming
/// the wrong key is worse than a page naming none.
pub const EXIT_HINT: &str = "Alt+m closes";

/// The action bar's pairs, verbatim from the mock, in the mock's order.
pub const ACTIONS: [(&str, &str); 8] = [
    ("a", "Add memory"),
    ("s", "Search"),
    ("p", "Pin"),
    ("u", "Unpin"),
    ("r", "Reindex"),
    ("x", "Archive"),
    ("R", "Refresh"),
    ("?", "Help"),
];

/// The six categories, closed set (plan M2), in the mock's order.
pub const CATEGORIES: [&str; 6] = [
    "Preferences",
    "Projects",
    "Facts",
    "Workflows",
    "People",
    "References",
];

/// The honest empty states, one dim line inside the exact card chrome.
pub const EMPTY_RECENT: &str = "No memories yet — [a] adds one";
pub const EMPTY_PINNED: &str = "Nothing pinned — [p] pins a memory";
pub const EMPTY_CONVO: &str = "No conversation yet — memory fills as you chat";
/// The status value when no index exists; the other rows show real zeros.
pub const NO_INDEX: &str = "No index yet — [r] builds one";

/// The query box's placeholder, verbatim from the mock.
/// The widest speaker label, so the transcript's text starts in one column.
///
/// Derived rather than written down: `Speaker` decides the labels, and a
/// literal here would be a second answer to "how wide is a name" that nothing
/// keeps in step. `const fn` because a `max` over two `&str` lengths is
/// something the compiler can do and a reader should not have to.
const SPEAKER_W: usize = {
    let (a, b) = ("Emma".len(), "You".len());
    if a > b {
        a
    } else {
        b
    }
};

pub const PLACEHOLDER: &str = "Ask Emma about your memory...";
/// The tip row's left half, verbatim from the mock (the bulb glyph is the
/// skin's; this is the text after it).
pub const TIP: &str = "Tip: Use search (/) to find anything in memory";

/// The sub-views' action bars: only the keys that act there.
pub const ALL_ACTIONS: [(&str, &str); 7] = [
    ("p", "Pin"),
    ("u", "Unpin"),
    ("x", "Archive"),
    ("f", "Filter"),
    ("b", "Back"),
    ("R", "Refresh"),
    ("?", "Help"),
];
pub const MANAGE_ACTIONS: [(&str, &str); 5] = [
    ("u", "Unpin"),
    ("x", "Archive"),
    ("b", "Back"),
    ("R", "Refresh"),
    ("?", "Help"),
];

/// The honest notices. Query and search have no retrieval stage behind them
/// yet (plan M2); saying so beats a silent key.
pub const NOTICE_M2: &str = "memory query needs the retrieval stage (M2)";
/// The store could not be read at all.
///
/// **A page rendering nothing and a page rendering a failure look alike and
/// mean opposite things.** Every count, card and empty state on this page is
/// built to describe an *empty* wiki — `EMPTY_RECENT`, `NO_INDEX`, six zero
/// counts — and [`super::app`]'s view builder returns exactly that shape when
/// `Wiki::project` or `Wiki::view` returns an error. So an unreadable store
/// renders as a store with nothing in it, and the reader is told the opposite
/// of what happened. The old text pages carried this sentence and the import
/// dropped it (F32, `notes/design/term-hardening-backport.md`); it names the
/// store as well as the failure, because the Harness page has its own and a
/// shared sentence would leave a reader unable to tell which one failed.
pub const NOTICE_UNREADABLE: &str =
    "the memory wiki under .emma/memory could not be read — not the same as empty";
pub const NOTICE_NO_ROW: &str = "No row selected — Tab and ↑/↓ select one";
/// The `[?]` help notices, one per key scope.
pub const HELP_MAIN: &str =
    "[a] add  [s] search  [p] pin  [u] unpin  [x] archive  [r] reindex  [R] refresh  [v] view all  Tab/↑/↓ move  Esc clears";
pub const HELP_ALL: &str =
    "[p] pin  [u] unpin  [x] archive  [f] filter  [b] back  [R] refresh  ↑/↓ select";
pub const HELP_MANAGE: &str = "[u] unpin  [x] archive  [b] back  [R] refresh  ↑/↓ select";
/// MANAGE PINNED's honest empty state (derived view, no mock).
pub const EMPTY_MANAGE: &str = "Nothing pinned — [p] pins a memory from the main page";
/// Where the add flow starts its category cycle: Facts, the owner's example
/// label (`category: facts — Tab cycles`).
pub const ADD_CATEGORY_START: usize = 2;

// endregion: State

// region: Keys
// ---------------------------------------------------------------------------
// Keys — the pure seam (plan M5)
// ---------------------------------------------------------------------------

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

/// What a key asks the shell to do. Everything that mutates the store crosses
/// this enum; the pure function below never touches a file, which is what
/// keeps it testable without a wiki and keeps disk IO out of the term layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemoryAction {
    /// Not this page's key (or a release): let it fall through.
    None,
    /// Re-read the wiki and rebuild the view.
    Refresh,
    Pin(String),
    Unpin(String),
    Archive(String),
    /// Create a memory: the typed text is title and body; the category is an
    /// index into [`CATEGORIES`].
    Add {
        title: String,
        category: usize,
    },
    /// Rebuild `index.md` from the pages on disk.
    Reindex,
    /// `[?]` fired; the view's notice was already toggled.
    Help,
    /// The query box submitted. Answering is M2's; the shell shows
    /// [`NOTICE_M2`] instead of pretending.
    Query(String),
    /// The view changed (focus, selection, typing, mode) and wants a repaint.
    FocusChanged,
}

/// One key, against the whole page. Two scopes, the owner's focus rule:
/// with the query box focused, printable keys type; otherwise the action-bar
/// keys act. Sub-views ([`PageMode`]) carry their own smaller key maps.
///
/// Alt/Ctrl chords and key releases are never this page's: they return
/// [`MemoryAction::None`] untouched so the global layer (Alt+m, Ctrl-C…)
/// keeps working over an open memory page.
pub fn handle_key(v: &mut MemoryView, key: KeyEvent) -> MemoryAction {
    if key.kind == KeyEventKind::Release
        || key
            .modifiers
            .intersects(KeyModifiers::ALT | KeyModifiers::CONTROL)
    {
        return MemoryAction::None;
    }
    if v.focus == Some(Focus::QueryBox) {
        return input_key(v, key);
    }
    match v.mode {
        PageMode::Main => main_key(v, key),
        PageMode::AllMemories => all_key(v, key),
        PageMode::ManagePinned => manage_key(v, key),
    }
}

/// The query box scope: typing, the add flow, submit, leave.
fn input_key(v: &mut MemoryView, key: KeyEvent) -> MemoryAction {
    match key.code {
        KeyCode::Esc => {
            // Cancel: the add flow drops its half-typed text; a plain query
            // keeps its text and only loses the focus.
            if v.adding.take().is_some() {
                v.query.clear();
            }
            v.focus = None;
            MemoryAction::FocusChanged
        }
        KeyCode::Tab => {
            if let Some(cat) = v.adding.as_mut() {
                *cat = (*cat + 1) % CATEGORIES.len();
            } else {
                // Out of the box and onto the card cycle.
                v.focus = Some(card_entry(v, 0));
            }
            MemoryAction::FocusChanged
        }
        KeyCode::Backspace => {
            v.query.pop();
            MemoryAction::FocusChanged
        }
        KeyCode::Enter => {
            let text = v.query.trim().to_string();
            if text.is_empty() {
                return MemoryAction::None;
            }
            match v.adding.take() {
                Some(category) => {
                    v.query.clear();
                    v.focus = None;
                    MemoryAction::Add {
                        title: text,
                        category,
                    }
                }
                None => MemoryAction::Query(text),
            }
        }
        KeyCode::Char(c) => {
            v.query.push(c);
            MemoryAction::FocusChanged
        }
        _ => MemoryAction::None,
    }
}

/// The main page, query box not focused: the action bar acts.
fn main_key(v: &mut MemoryView, key: KeyEvent) -> MemoryAction {
    match key.code {
        KeyCode::Char('a') => {
            v.adding = Some(ADD_CATEGORY_START);
            v.query.clear();
            v.focus = Some(Focus::QueryBox);
            MemoryAction::FocusChanged
        }
        KeyCode::Char('s') => {
            v.notice = Some(NOTICE_M2.to_string());
            MemoryAction::FocusChanged
        }
        KeyCode::Char('p') => selected_or_ask(v, MemoryAction::Pin),
        KeyCode::Char('u') => selected_or_ask(v, MemoryAction::Unpin),
        KeyCode::Char('x') => selected_or_ask(v, MemoryAction::Archive),
        KeyCode::Char('r') => MemoryAction::Reindex,
        KeyCode::Char('R') => MemoryAction::Refresh,
        KeyCode::Char('?') => {
            toggle_help(v, HELP_MAIN);
            MemoryAction::Help
        }
        KeyCode::Char('v') => {
            open_all(v);
            MemoryAction::FocusChanged
        }
        KeyCode::Enter => match v.focus {
            Some(Focus::RecentViewAll) => {
                open_all(v);
                MemoryAction::FocusChanged
            }
            Some(Focus::PinnedManage) => {
                open_manage(v);
                MemoryAction::FocusChanged
            }
            _ => MemoryAction::None,
        },
        KeyCode::Tab => {
            let next = v.focus.map_or(0, |f| (card_of(f) + 1) % 6);
            v.focus = Some(card_entry(v, next));
            MemoryAction::FocusChanged
        }
        KeyCode::Up => step_within(v, -1),
        KeyCode::Down => step_within(v, 1),
        KeyCode::Esc => {
            v.focus = None;
            v.notice = None;
            MemoryAction::FocusChanged
        }
        _ => MemoryAction::None,
    }
}

/// The ALL MEMORIES sub-view's keys.
fn all_key(v: &mut MemoryView, key: KeyEvent) -> MemoryAction {
    let shown = filtered_all(v);
    match key.code {
        KeyCode::Up => {
            v.all_selected = v.all_selected.saturating_sub(1);
            MemoryAction::FocusChanged
        }
        KeyCode::Down => {
            v.all_selected = (v.all_selected + 1).min(shown.len().saturating_sub(1));
            MemoryAction::FocusChanged
        }
        KeyCode::Char('p') => all_selected_or_ask(v, &shown, MemoryAction::Pin),
        KeyCode::Char('u') => all_selected_or_ask(v, &shown, MemoryAction::Unpin),
        KeyCode::Char('x') => all_selected_or_ask(v, &shown, MemoryAction::Archive),
        KeyCode::Char('f') => {
            v.all_filter = match v.all_filter {
                None => Some(0),
                Some(i) if i + 1 < CATEGORIES.len() => Some(i + 1),
                Some(_) => None,
            };
            v.all_selected = 0;
            MemoryAction::FocusChanged
        }
        KeyCode::Char('b') | KeyCode::Esc => {
            v.mode = PageMode::Main;
            v.notice = None;
            MemoryAction::FocusChanged
        }
        KeyCode::Char('R') => MemoryAction::Refresh,
        KeyCode::Char('r') => MemoryAction::Reindex,
        KeyCode::Char('?') => {
            toggle_help(v, HELP_ALL);
            MemoryAction::Help
        }
        _ => MemoryAction::None,
    }
}

/// The MANAGE PINNED sub-view's keys.
fn manage_key(v: &mut MemoryView, key: KeyEvent) -> MemoryAction {
    match key.code {
        KeyCode::Up => {
            v.pin_selected = v.pin_selected.saturating_sub(1);
            MemoryAction::FocusChanged
        }
        KeyCode::Down => {
            v.pin_selected = (v.pin_selected + 1).min(v.pinned.len().saturating_sub(1));
            MemoryAction::FocusChanged
        }
        KeyCode::Char('u') => match v.pinned.get(v.pin_selected) {
            Some(row) => MemoryAction::Unpin(row.slug.clone()),
            None => MemoryAction::None,
        },
        KeyCode::Char('x') => match v.pinned.get(v.pin_selected) {
            Some(row) => MemoryAction::Archive(row.slug.clone()),
            None => MemoryAction::None,
        },
        KeyCode::Char('b') | KeyCode::Esc => {
            v.mode = PageMode::Main;
            v.notice = None;
            MemoryAction::FocusChanged
        }
        KeyCode::Char('R') => MemoryAction::Refresh,
        KeyCode::Char('?') => {
            toggle_help(v, HELP_MANAGE);
            MemoryAction::Help
        }
        _ => MemoryAction::None,
    }
}

fn open_all(v: &mut MemoryView) {
    v.mode = PageMode::AllMemories;
    v.all_selected = 0;
    v.notice = None;
}

fn open_manage(v: &mut MemoryView) {
    v.mode = PageMode::ManagePinned;
    v.pin_selected = 0;
    v.notice = None;
}

/// Indices into [`MemoryView::all`] that pass the category filter.
pub fn filtered_all(v: &MemoryView) -> Vec<usize> {
    v.all
        .iter()
        .enumerate()
        .filter(|(_, r)| v.all_filter.is_none_or(|c| c == r.category))
        .map(|(i, _)| i)
        .collect()
}

/// The slug the main page's selection names, if a row is selected.
fn main_selection(v: &MemoryView) -> Option<String> {
    match v.focus? {
        Focus::RecentRow(i) => v.recent.get(i).map(|r| r.slug.clone()),
        Focus::PinnedDots(i) => v.pinned.get(i).map(|r| r.slug.clone()),
        _ => None,
    }
}

/// An action on the main page's selection — or the honest ask for one.
fn selected_or_ask(v: &mut MemoryView, make: fn(String) -> MemoryAction) -> MemoryAction {
    match main_selection(v) {
        Some(slug) => make(slug),
        None => {
            v.notice = Some(NOTICE_NO_ROW.to_string());
            MemoryAction::FocusChanged
        }
    }
}

/// The same, on ALL MEMORIES' filtered selection.
fn all_selected_or_ask(
    v: &mut MemoryView,
    shown: &[usize],
    make: fn(String) -> MemoryAction,
) -> MemoryAction {
    match shown.get(v.all_selected).and_then(|&i| v.all.get(i)) {
        Some(row) => make(row.slug.clone()),
        None => {
            v.notice = Some(NOTICE_NO_ROW.to_string());
            MemoryAction::FocusChanged
        }
    }
}

fn toggle_help(v: &mut MemoryView, text: &str) {
    if v.notice.as_deref() == Some(text) {
        v.notice = None;
    } else {
        v.notice = Some(text.to_string());
    }
}

/// Which of the six Tab stops a focus belongs to. The cycle order — recorded
/// in `notes/design-memory-page.md` — is: Recent, Pinned, Conversation,
/// Retrieval, Categories, Query box, then around again.
fn card_of(f: Focus) -> usize {
    match f {
        Focus::RecentRow(_) | Focus::RecentViewAll => 0,
        Focus::PinnedDots(_) | Focus::PinnedManage => 1,
        Focus::ConvoHistory => 2,
        Focus::Retrieval => 3,
        Focus::CategoryBox(_) => 4,
        Focus::QueryBox => 5,
    }
}

/// Where Tab lands inside card `i`: the first row when the card has rows,
/// its header affordance when it does not.
fn card_entry(v: &MemoryView, i: usize) -> Focus {
    match i {
        0 if v.recent.is_empty() => Focus::RecentViewAll,
        0 => Focus::RecentRow(0),
        1 if v.pinned.is_empty() => Focus::PinnedManage,
        1 => Focus::PinnedDots(0),
        2 => Focus::ConvoHistory,
        3 => Focus::Retrieval,
        4 => Focus::CategoryBox(0),
        _ => Focus::QueryBox,
    }
}

/// ↑/↓ inside the focused card. Recent and Pinned put their header
/// affordance above row 0; the category boxes step sideways through their
/// row of six; single-target cards have nowhere to go.
fn step_within(v: &mut MemoryView, dir: isize) -> MemoryAction {
    let next = match (v.focus, dir) {
        (Some(Focus::RecentViewAll), 1) if !v.recent.is_empty() => Some(Focus::RecentRow(0)),
        (Some(Focus::RecentRow(0)), -1) => Some(Focus::RecentViewAll),
        (Some(Focus::RecentRow(i)), 1) if i + 1 < v.recent.len() => Some(Focus::RecentRow(i + 1)),
        (Some(Focus::RecentRow(i)), -1) => Some(Focus::RecentRow(i - 1)),
        (Some(Focus::PinnedManage), 1) if !v.pinned.is_empty() => Some(Focus::PinnedDots(0)),
        (Some(Focus::PinnedDots(0)), -1) => Some(Focus::PinnedManage),
        (Some(Focus::PinnedDots(i)), 1) if i + 1 < v.pinned.len() => Some(Focus::PinnedDots(i + 1)),
        (Some(Focus::PinnedDots(i)), -1) => Some(Focus::PinnedDots(i - 1)),
        (Some(Focus::CategoryBox(i)), 1) if i + 1 < CATEGORIES.len() => {
            Some(Focus::CategoryBox(i + 1))
        }
        (Some(Focus::CategoryBox(i)), -1) if i > 0 => Some(Focus::CategoryBox(i - 1)),
        _ => None,
    };
    match next {
        Some(f) => {
            v.focus = Some(f);
            MemoryAction::FocusChanged
        }
        None => MemoryAction::None,
    }
}

// endregion: Keys

// region: Rendering
// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

/// The page's own glyphs, one ASCII fallback each — the `render::ASCII`
/// split, extended for shapes that file does not carry.
struct PageGlyphs {
    doc: &'static str,
    chat: &'static str,
    pin: &'static str,
    dots: &'static str,
    check: &'static str,
    cross: &'static str,
    arrow: &'static str,
    bulb: &'static str,
    dot_on: &'static str,
    dot_off: &'static str,
    full: &'static str,
    empty: &'static str,
    cat: [&'static str; 6],
}

fn page_glyphs(skin: &Skin) -> PageGlyphs {
    if skin.glyphs == ASCII {
        PageGlyphs {
            doc: "=",
            chat: "*",
            pin: "+",
            dots: "...",
            check: "+",
            cross: "x",
            arrow: "->",
            bulb: "(!)",
            dot_on: "*",
            dot_off: "o",
            full: "#",
            empty: "-",
            cat: ["*", "#", "=", "~", "&", "%"],
        }
    } else {
        PageGlyphs {
            doc: "▤",
            chat: "◆",
            pin: "⚑",
            dots: "···",
            check: "✓",
            cross: "✗",
            arrow: "→",
            bulb: "💡",
            dot_on: "●",
            dot_off: "○",
            full: "█",
            empty: "░",
            cat: ["⚙", "▦", "◇", "↻", "☺", "§"],
        }
    }
}

/// The honest empty-state strings carry an em dash; a legacy code page gets
/// a hyphen instead.
fn honest(text: &str, ascii: bool) -> String {
    if ascii {
        text.replace('—', "-")
    } else {
        text.to_string()
    }
}

/// Draw the whole page into `area`.
///
/// The tip row and the query box are **bottom-anchored**, exactly like the
/// chat input box: the owner's 2026-08-26 screenshot showed a short terminal
/// losing them because they rendered last, top-down. The card grid gets the
/// remaining height and compresses honestly — see [`band_heights`].
pub fn render(area: Rect, buf: &mut Buffer, v: &MemoryView, skin: &Skin) {
    if area.width < 4 || area.height < 2 {
        return;
    }
    let g = page_glyphs(skin);
    match v.mode {
        PageMode::Main => render_main(area, buf, v, skin, &g),
        PageMode::AllMemories => render_all(area, buf, v, skin, &g),
        PageMode::ManagePinned => render_manage(area, buf, v, skin, &g),
    }
}

/// The mock's page: card grid over the bottom-anchored tip row + query box.
fn render_main(area: Rect, buf: &mut Buffer, v: &MemoryView, skin: &Skin, g: &PageGlyphs) {
    // The anchor: query box (3) at the very bottom, tip row (1) above it,
    // the rest is the grid's. Under 6 rows there is no grid worth arguing
    // over — the query box still wins, because it is the page's one input.
    let (content, tip_area, query_area) = if area.height >= 6 {
        let [c, t, q] = Layout::vertical([
            Constraint::Fill(1),
            Constraint::Length(1),
            Constraint::Length(3),
        ])
        .areas(area);
        (c, Some(t), Some(q))
    } else if area.height >= 3 {
        let [c, q] = Layout::vertical([Constraint::Fill(1), Constraint::Length(3)]).areas(area);
        (c, None, Some(q))
    } else {
        (area, None, None)
    };

    render_grid(content, buf, v, skin, g);
    if let Some(t) = tip_area {
        render_tip(t, buf, v, skin, g);
    }
    if let Some(q) = query_area {
        render_query(q, buf, v, skin);
    }
}

/// The two derived sub-views share one skeleton: head, their own action bar,
/// one full-width listing card, and the bottom-anchored notice/hint row.
/// Derived designs (no mock): the main page's idiom, applied — recorded in
/// `notes/design-memory-page.md`.
#[allow(clippy::too_many_arguments)]
fn render_sub(
    area: Rect,
    buf: &mut Buffer,
    v: &MemoryView,
    skin: &Skin,
    title: &str,
    subtitle: &str,
    actions: &[(&str, &str)],
    card: impl FnOnce(Rect, &mut Buffer),
) {
    let w = usize::from(area.width);
    let (content, hint_area) = if area.height >= 4 {
        let [c, h] = Layout::vertical([Constraint::Fill(1), Constraint::Length(1)]).areas(area);
        (c, Some(h))
    } else {
        (area, None)
    };
    let bottom = content.y + content.height;
    let mut y = content.y;
    let version = fit(&v.version, w, skin.glyphs.ellipsis);
    let head = [
        Line::from(vec![
            Span::raw(" ".repeat(w.saturating_sub(cols(&version)))),
            Span::styled(version, skin.palette.dim()),
        ]),
        Line::from(Span::styled(
            fit(title, w, skin.glyphs.ellipsis),
            skin.palette.bold(Role::Accent),
        )),
        Line::from(Span::styled(
            fit(subtitle, w, skin.glyphs.ellipsis),
            skin.palette.dim(),
        )),
        Line::from(Span::styled(skin.glyphs.rule.repeat(w), skin.palette.dim())),
    ];
    for line in head {
        if y >= bottom {
            return;
        }
        buf.set_line(content.x, y, &line, content.width);
        y += 1;
    }
    if y >= bottom {
        return;
    }
    buf.set_line(content.x, y, &action_bar(actions, skin), content.width);
    y += 2;
    if y < bottom {
        card(Rect::new(content.x, y, content.width, bottom - y), buf);
    }
    if let Some(h) = hint_area {
        let text = v
            .notice
            .clone()
            .unwrap_or_else(|| "[b] back to Memory".to_string());
        let style = if v.notice.is_some() {
            skin.palette.style(Role::Accent)
        } else {
            skin.palette.dim()
        };
        let line = Line::from(Span::styled(fit(&text, w, skin.glyphs.ellipsis), style));
        buf.set_line(h.x, h.y, &line, h.width);
    }
}

/// One listing row: pin glyph when pinned, title, then category and created
/// date right-aligned dim. The selected row wears the chip band.
fn listing_row(r: &MemRow, selected: bool, w: usize, skin: &Skin, g: &PageGlyphs) -> Line<'static> {
    let lead = if r.pinned {
        format!("{} ", g.pin)
    } else {
        "  ".to_string()
    };
    let mut line = lr(
        vec![
            Span::styled(lead, skin.palette.style(Role::Accent)),
            Span::styled(r.title.clone(), skin.palette.style(Role::Text)),
        ],
        vec![Span::styled(
            format!("{}  {}", CATEGORIES[r.category.min(5)], r.created),
            skin.palette.dim(),
        )],
        w,
        skin,
    );
    if selected {
        line = banded(line, skin.palette.chip(Role::Accent));
    }
    line
}

/// A selection-following window: the selected row stays visible, the header
/// says `N of M`, and rows above scroll away rather than truncate.
fn window(len: usize, selected: usize, viewport: usize) -> std::ops::Range<usize> {
    if viewport == 0 || len == 0 {
        return 0..0;
    }
    let top = (selected + 1)
        .saturating_sub(viewport)
        .min(len.saturating_sub(viewport));
    top..(top + viewport).min(len)
}

/// ALL MEMORIES: the full live listing, filterable by category.
fn render_all(area: Rect, buf: &mut Buffer, v: &MemoryView, skin: &Skin, g: &PageGlyphs) {
    render_sub(
        area,
        buf,
        v,
        skin,
        "All Memories",
        "Every live memory in this project's wiki",
        &ALL_ACTIONS,
        |card_area, buf| {
            let shown = filtered_all(v);
            let sel = v.all_selected.min(shown.len().saturating_sub(1));
            let fname = v.all_filter.map_or("All", |i| CATEGORIES[i.min(5)]);
            let mut right = format!("Filter: {fname}");
            if v.all_filter.is_some() {
                right.push_str(&format!(" ({} of {})", shown.len(), v.all.len()));
            }
            if !shown.is_empty() {
                right.push_str(&format!("   {} of {}", sel + 1, shown.len()));
            }
            let content = card_frame(
                card_area,
                buf,
                skin,
                false,
                "ALL MEMORIES",
                vec![Span::styled(right, skin.palette.dim())],
            );
            let w = usize::from(content.width);
            if shown.is_empty() {
                let text = match v.all_filter {
                    None => honest(EMPTY_RECENT, skin.glyphs == ASCII),
                    Some(i) => honest(
                        &format!(
                            "No {} memories — [f] cycles the filter",
                            CATEGORIES[i.min(5)]
                        ),
                        skin.glyphs == ASCII,
                    ),
                };
                set_row(
                    content,
                    0,
                    buf,
                    Line::from(Span::styled(
                        fit(&text, w, skin.glyphs.ellipsis),
                        skin.palette.dim(),
                    )),
                );
                return;
            }
            for (row_i, i) in window(shown.len(), sel, usize::from(content.height)).enumerate() {
                let r = &v.all[shown[i]];
                set_row(
                    content,
                    row_i as u16,
                    buf,
                    listing_row(r, i == sel, w, skin, g),
                );
            }
        },
    );
}

/// MANAGE PINNED: the pinned-only listing.
fn render_manage(area: Rect, buf: &mut Buffer, v: &MemoryView, skin: &Skin, g: &PageGlyphs) {
    render_sub(
        area,
        buf,
        v,
        skin,
        "Manage Pinned",
        "Pinned facts — unpin or archive them here",
        &MANAGE_ACTIONS,
        |card_area, buf| {
            let sel = v.pin_selected.min(v.pinned.len().saturating_sub(1));
            let right = if v.pinned.is_empty() {
                String::new()
            } else {
                format!("{} of {}", sel + 1, v.pinned.len())
            };
            let content = card_frame(
                card_area,
                buf,
                skin,
                false,
                &format!("MANAGE PINNED ({})", v.pinned.len()),
                vec![Span::styled(right, skin.palette.dim())],
            );
            let w = usize::from(content.width);
            if v.pinned.is_empty() {
                set_row(content, 0, buf, empty_line(EMPTY_MANAGE, w, skin));
                return;
            }
            for (row_i, i) in window(v.pinned.len(), sel, usize::from(content.height)).enumerate() {
                set_row(
                    content,
                    row_i as u16,
                    buf,
                    listing_row(&v.pinned[i], i == sel, w, skin, g),
                );
            }
        },
    );
}

/// Head, action bar, and the card grid, into the space the anchor left over.
fn render_grid(area: Rect, buf: &mut Buffer, v: &MemoryView, skin: &Skin, g: &PageGlyphs) {
    if area.width < 4 || area.height == 0 {
        return;
    }
    let w = usize::from(area.width);
    let bottom = area.y + area.height;
    let mut y = area.y;

    // The head: version in the corner, the title, the subtitle, a rule.
    // "Very large" is not a thing a terminal cell can do; one bold accent row
    // is this repository's standing substitute (settings design Q8).
    let head = [
        corner_row(EXIT_HINT, &v.version, w, skin),
        Line::from(Span::styled(
            fit("Memory", w, skin.glyphs.ellipsis),
            skin.palette.bold(Role::Accent),
        )),
        Line::from(Span::styled(
            fit(SUBTITLE, w, skin.glyphs.ellipsis),
            skin.palette.dim(),
        )),
        Line::from(Span::styled(skin.glyphs.rule.repeat(w), skin.palette.dim())),
    ];
    for line in head {
        if y >= bottom {
            return;
        }
        buf.set_line(area.x, y, &line, area.width);
        y += 1;
    }

    // The action bar: bracketed accent key, dim label, three columns of air.
    if y >= bottom {
        return;
    }
    buf.set_line(area.x, y, &action_bar(&ACTIONS, skin), area.width);
    y += 2; // the bar, then one row of air before the grid

    // The grid columns, one column of air between them.
    let grid = Rect::new(area.x, y.min(bottom), area.width, bottom.saturating_sub(y));
    let [lc, _, rc] = Layout::horizontal([
        Constraint::Fill(1),
        Constraint::Length(1),
        Constraint::Fill(1),
    ])
    .areas(grid);

    // Desired heights: each pair shares a band as tall as its taller card,
    // the settings grid's rule; categories are header + five-row mini-boxes.
    let recent_h = v.recent.len().max(1) as u16 + 3;
    let pinned_h = v.pinned.len().max(1) as u16 + 5;
    let footer = u16::from(!v.transcript.is_empty());
    let convo_h = v.transcript.len().max(1) as u16 + footer + 3;
    let [h1, h2, h3] = band_heights(grid.height, [recent_h.max(pinned_h), convo_h.max(11), 8]);

    if h1 >= 3 {
        render_recent(Rect::new(lc.x, y, lc.width, h1), buf, v, skin, g);
        render_pinned(Rect::new(rc.x, y, rc.width, h1), buf, v, skin, g);
        y += h1;
    }
    if h2 >= 3 {
        render_convo(Rect::new(lc.x, y, lc.width, h2), buf, v, skin, g);
        render_retrieval(Rect::new(rc.x, y, rc.width, h2), buf, v, skin, g);
        y += h2;
    }
    if h3 >= 3 {
        render_categories(Rect::new(area.x, y, area.width, h3), buf, v, skin, g);
    }
}

/// Divide the grid's height across the three card bands.
///
/// Enough room means everyone gets what they asked for. Short means every
/// band keeps at least a header and one honest row (4: borders + header +
/// `… N more`), the categories keep their full 8 or drop to the fallback
/// height, and the spare goes to the top bands first — recency outranks
/// status. Truly tiny degrades top-down, the old behavior, because five
/// headers with no rows say less than two whole cards.
fn band_heights(total: u16, desired: [u16; 3]) -> [u16; 3] {
    let [d1, d2, d3] = desired;
    if d1 + d2 + d3 <= total {
        return desired;
    }
    for cat in [d3.min(8), 4] {
        let floor = 4 + 4 + cat;
        if floor <= total {
            let mut h = [4, 4, cat];
            let mut spare = total - floor;
            for i in 0..2 {
                let grow = (desired[i] - h[i]).min(spare);
                h[i] += grow;
                spare -= grow;
            }
            return h;
        }
    }
    // Sequential clip, top card first.
    let h1 = d1.min(total);
    let h2 = d2.min(total - h1);
    [h1, h2, d3.min(total - h1 - h2)]
}

/// The action bar for a key set: bracketed accent key, dim label.
fn action_bar(actions: &[(&str, &str)], skin: &Skin) -> Line<'static> {
    let mut bar: Vec<Span<'static>> = Vec::new();
    for (i, (key, label)) in actions.iter().enumerate() {
        if i > 0 {
            bar.push(Span::raw("   "));
        }
        bar.push(Span::styled(
            format!("[{key}]"),
            skin.palette.style(Role::Accent),
        ));
        bar.push(Span::styled(format!(" {label}"), skin.palette.dim()));
    }
    Line::from(bar)
}

/// The tip row — or the one-line notice, when the page has something to say.
fn render_tip(area: Rect, buf: &mut Buffer, v: &MemoryView, skin: &Skin, g: &PageGlyphs) {
    let w = usize::from(area.width);
    if let Some(n) = &v.notice {
        let line = Line::from(Span::styled(
            fit(n, w, skin.glyphs.ellipsis),
            skin.palette.style(Role::Accent),
        ));
        buf.set_line(area.x, area.y, &line, area.width);
        return;
    }
    let left = vec![
        Span::styled(format!("{} ", g.bulb), skin.palette.dim()),
        Span::styled(TIP.to_string(), skin.palette.dim()),
    ];
    let right = if v.auto_save {
        vec![
            Span::styled(g.dot_on.to_string(), skin.palette.style(Role::Ok)),
            Span::styled(
                " Auto-save enabled".to_string(),
                skin.palette.style(Role::Text),
            ),
        ]
    } else {
        vec![Span::styled(
            format!("{} Auto-save off", g.dot_off),
            skin.palette.dim(),
        )]
    };
    buf.set_line(area.x, area.y, &lr(left, right, w, skin), area.width);
}

// -- the cards ---------------------------------------------------------------

/// One bordered card: accent border when its card holds the focus, then the
/// header line (left bold accent, right the card's affordance). Returns the
/// content area under the header.
fn card_frame(
    area: Rect,
    buf: &mut Buffer,
    skin: &Skin,
    focused: bool,
    left: &str,
    right: Vec<Span<'static>>,
) -> Rect {
    let block = Block::bordered()
        .border_set(skin.glyphs.border)
        .border_style(if focused {
            skin.palette.style(Role::Accent)
        } else {
            skin.palette.dim()
        });
    let inner = block.inner(area);
    block.render(area, buf);
    if inner.height == 0 || inner.width == 0 {
        return Rect::new(inner.x, inner.y, inner.width, 0);
    }
    let header = lr(
        vec![Span::styled(
            left.to_string(),
            skin.palette.bold(Role::Accent),
        )],
        right,
        usize::from(inner.width),
        skin,
    );
    buf.set_line(inner.x, inner.y, &header, inner.width);
    Rect::new(inner.x, inner.y + 1, inner.width, inner.height - 1)
}

fn render_recent(area: Rect, buf: &mut Buffer, v: &MemoryView, skin: &Skin, g: &PageGlyphs) {
    if area.width < 4 || area.height < 3 {
        return;
    }
    let focus = v.focus;
    let focused = matches!(focus, Some(Focus::RecentRow(_) | Focus::RecentViewAll));
    let affordance = affordance_span(
        format!("View all {}", g.arrow),
        matches!(focus, Some(Focus::RecentViewAll)),
        skin,
    );
    let content = card_frame(
        area,
        buf,
        skin,
        focused,
        "RECENT MEMORIES",
        vec![affordance],
    );
    let w = usize::from(content.width);
    if v.recent.is_empty() {
        set_row(content, 0, buf, empty_line(EMPTY_RECENT, w, skin));
        return;
    }
    let shown = fits(v.recent.len(), content.height);
    for (i, r) in v.recent.iter().take(shown).enumerate() {
        let glyph = match r.source {
            Source::Doc => g.doc,
            Source::Chat => g.chat,
        };
        let mut line = lr(
            vec![
                Span::styled(format!("{glyph} "), skin.palette.style(Role::Accent)),
                Span::styled(r.text.clone(), skin.palette.style(Role::Text)),
            ],
            vec![Span::styled(r.time.clone(), skin.palette.dim())],
            w,
            skin,
        );
        if focus == Some(Focus::RecentRow(i)) {
            line = banded(line, skin.palette.chip(Role::Accent));
        }
        set_row(content, i as u16, buf, line);
    }
    if shown < v.recent.len() {
        set_row(
            content,
            shown as u16,
            buf,
            more_line(v.recent.len() - shown, w, skin),
        );
    }
}

/// How many of `rows` fit in `height`, keeping one row for `… N more` when
/// they do not all fit. Zero height shows nothing, honestly nothing.
fn fits(rows: usize, height: u16) -> usize {
    let h = usize::from(height);
    if rows <= h {
        rows
    } else {
        h.saturating_sub(1)
    }
}

/// The honest truncation marker: a card too short for its rows says so.
fn more_line(hidden: usize, w: usize, skin: &Skin) -> Line<'static> {
    Line::from(Span::styled(
        fit(
            &format!("{} {hidden} more", skin.glyphs.ellipsis),
            w,
            skin.glyphs.ellipsis,
        ),
        skin.palette.dim(),
    ))
}

fn render_pinned(area: Rect, buf: &mut Buffer, v: &MemoryView, skin: &Skin, g: &PageGlyphs) {
    if area.width < 6 || area.height < 3 {
        return;
    }
    let focus = v.focus;
    let focused = matches!(focus, Some(Focus::PinnedDots(_) | Focus::PinnedManage));
    let affordance = affordance_span(
        format!("Manage {}", g.arrow),
        matches!(focus, Some(Focus::PinnedManage)),
        skin,
    );
    let header = format!("PINNED FACTS ({})", v.pinned.len());
    let content = card_frame(area, buf, skin, focused, &header, vec![affordance]);
    if content.height < 3 {
        // Too short even for the inner border: say what is hidden.
        if !v.pinned.is_empty() {
            set_row(
                content,
                0,
                buf,
                more_line(v.pinned.len(), usize::from(content.width), skin),
            );
        }
        return;
    }
    // The mock draws the pin rows inside their own inner border.
    let block = Block::bordered()
        .border_set(skin.glyphs.border)
        .border_style(skin.palette.dim());
    let inner = block.inner(content);
    block.render(content, buf);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let w = usize::from(inner.width);
    if v.pinned.is_empty() {
        set_row(inner, 0, buf, empty_line(EMPTY_PINNED, w, skin));
        return;
    }
    let shown = fits(v.pinned.len(), inner.height);
    for (i, fact) in v.pinned.iter().take(shown).enumerate() {
        let dots = Span::styled(
            g.dots.to_string(),
            if focus == Some(Focus::PinnedDots(i)) {
                skin.palette.chip(Role::Accent)
            } else {
                skin.palette.dim()
            },
        );
        let line = lr(
            vec![
                Span::styled(format!("{} ", g.pin), skin.palette.style(Role::Accent)),
                Span::styled(fact.title.clone(), skin.palette.style(Role::Text)),
            ],
            vec![dots],
            w,
            skin,
        );
        set_row(inner, i as u16, buf, line);
    }
    if shown < v.pinned.len() {
        set_row(
            inner,
            shown as u16,
            buf,
            more_line(v.pinned.len() - shown, w, skin),
        );
    }
}

fn render_convo(area: Rect, buf: &mut Buffer, v: &MemoryView, skin: &Skin, g: &PageGlyphs) {
    if area.width < 4 || area.height < 3 {
        return;
    }
    let focus = v.focus;
    let tokens = if v.convo_tokens.is_empty() {
        "0"
    } else {
        &v.convo_tokens
    };
    let counts = Span::styled(
        format!("Tokens: {tokens}   Messages: {}", v.convo_messages),
        skin.palette.dim(),
    );
    let focused = matches!(focus, Some(Focus::ConvoHistory));
    let content = card_frame(
        area,
        buf,
        skin,
        focused,
        "CONVERSATION MEMORY",
        vec![counts],
    );
    let w = usize::from(content.width);
    if v.transcript.is_empty() {
        set_row(content, 0, buf, empty_line(EMPTY_CONVO, w, skin));
        return;
    }
    let shown = fits(v.transcript.len(), content.height);
    for (i, (speaker, text)) in v.transcript.iter().take(shown).enumerate() {
        let (name, style) = match speaker {
            Speaker::Emma => ("Emma", skin.palette.style(Role::Accent)),
            Speaker::You => ("You", skin.palette.bold(Role::Text)),
        };
        // **Padded to the widest speaker, so the words start in one column.**
        // `Emma` is four columns and `You` is three, so a fixed two-space gap
        // put every other line's text one column left of its neighbour's — a
        // ragged left edge down the middle of the card, which is what the
        // owner saw first. Measured from the labels rather than written as a
        // literal: a third speaker, or a translated one, moves the column
        // without anybody remembering this line exists.
        let label_w = SPEAKER_W + 2;
        let line = Line::from(vec![
            Span::styled(format!("{name:<pad$}  ", pad = SPEAKER_W), style),
            Span::styled(
                clip(text, w.saturating_sub(label_w), skin),
                skin.palette.style(Role::Text),
            ),
        ]);
        set_row(content, i as u16, buf, line);
    }
    if shown < v.transcript.len() {
        set_row(
            content,
            shown as u16,
            buf,
            more_line(v.transcript.len() - shown, w, skin),
        );
        return; // no room left for the footer either
    }
    // The footer sinks to the card's last inner row, like a settings card's
    // description: archival claim left (only when true), history right.
    let left = if v.older_archived {
        vec![Span::styled(
            "Older messages archived".to_string(),
            skin.palette.dim().add_modifier(Modifier::ITALIC),
        )]
    } else {
        Vec::new()
    };
    let right = affordance_span(
        format!("View full history {}", g.arrow),
        matches!(focus, Some(Focus::ConvoHistory)),
        skin,
    );
    if content.height > v.transcript.len() as u16 {
        set_row(
            content,
            content.height - 1,
            buf,
            lr(left, vec![right], w, skin),
        );
    }
}

fn render_retrieval(area: Rect, buf: &mut Buffer, v: &MemoryView, skin: &Skin, g: &PageGlyphs) {
    if area.width < 4 || area.height < 3 {
        return;
    }
    let focused = matches!(v.focus, Some(Focus::Retrieval));
    let content = card_frame(area, buf, skin, focused, "RETRIEVAL STATUS", Vec::new());
    let w = usize::from(content.width);
    let ascii = skin.glyphs == ASCII;
    let accent = skin.palette.style(Role::Accent);
    let dim = skin.palette.dim();
    let status = match &v.index {
        Some(ix) if ix.healthy => {
            Span::styled(format!("{} Healthy", g.check), skin.palette.style(Role::Ok))
        }
        Some(_) => Span::styled(
            format!("{} Unhealthy", g.cross),
            skin.palette.style(Role::Err),
        ),
        None => Span::styled(honest(NO_INDEX, ascii), dim),
    };
    let ix = v.index.as_ref();
    let val = |live: Option<String>, fallback: &str| match live {
        Some(t) => Span::styled(t, accent),
        None => Span::styled(fallback.to_string(), dim),
    };
    let rows: Vec<(&str, Span<'static>)> = vec![
        ("Index status", status),
        (
            "Last updated",
            val(ix.map(|i| i.last_updated.clone()), "never"),
        ),
        (
            "Total memories",
            Span::styled(v.total_memories.to_string(), accent),
        ),
        (
            "Indexed tokens",
            val(ix.map(|i| i.indexed_tokens.clone()), "0"),
        ),
        (
            "Embedding model",
            Span::styled(v.embedding_model.clone(), accent),
        ),
        (
            "Top score average",
            val(ix.map(|i| i.top_score_avg.clone()), "n/a"),
        ),
        (
            "Retrieval latency",
            val(ix.map(|i| i.latency.clone()), "n/a"),
        ),
    ];
    // Eight rows in all: seven labels and the gauge. A shorter card shows
    // what fits and one honest `… N more` in place of the rest.
    let shown = fits(rows.len() + 1, content.height);
    let mut i = 0u16;
    for (label, value) in rows.into_iter().take(shown) {
        let line = lr(
            vec![Span::styled(
                label.to_string(),
                skin.palette.style(Role::Text),
            )],
            vec![value],
            w,
            skin,
        );
        set_row(content, i, buf, line);
        i += 1;
    }
    if shown < 8 {
        set_row(content, i, buf, more_line(8 - shown, w, skin));
        return;
    }
    // The coverage gauge: the status bar's idiom, ten segments, percent
    // right-aligned. No index is a real zero, not a sample.
    let pct = ix.map(|x| u64::from(x.coverage_pct.min(100))).unwrap_or(0);
    let filled = if pct >= 100 {
        10
    } else {
        (pct as usize * 10).div_ceil(100).min(9)
    };
    let mut left = vec![Span::styled(
        "Index coverage ".to_string(),
        skin.palette.style(Role::Text),
    )];
    if filled > 0 {
        left.push(Span::styled(g.full.repeat(filled), accent));
    }
    if filled < 10 {
        left.push(Span::styled(g.empty.repeat(10 - filled), dim));
    }
    set_row(
        content,
        i,
        buf,
        lr(left, vec![Span::styled(format!("{pct}%"), accent)], w, skin),
    );
}

fn render_categories(area: Rect, buf: &mut Buffer, v: &MemoryView, skin: &Skin, g: &PageGlyphs) {
    if area.width < 20 || area.height < 3 {
        return;
    }
    let focus = v.focus;
    let focused_card = matches!(focus, Some(Focus::CategoryBox(_)));
    let content = card_frame(
        area,
        buf,
        skin,
        focused_card,
        "MEMORY CATEGORIES",
        Vec::new(),
    );
    // The mini-box geometry (owner defect, 2026-08-26): five rows — border,
    // glyph+name bold on one line, count under it, a row of air, border. A
    // glyph is one cell and cannot grow; the air and the merged bold line are
    // what presence a terminal can give. Less than the full five rows renders
    // the honest one-line fallback instead of a squashed half-box.
    if content.height < 5 {
        if content.height >= 1 {
            set_row(
                content,
                0,
                buf,
                more_line(CATEGORIES.len(), usize::from(content.width), skin),
            );
        }
        return;
    }
    let band = Rect::new(content.x, content.y, content.width, 5);
    let boxes: [Rect; 6] = Layout::horizontal([Constraint::Fill(1); 6]).areas(band);
    for (i, name) in CATEGORIES.iter().enumerate() {
        let hot = focus == Some(Focus::CategoryBox(i));
        let block = Block::bordered()
            .border_set(skin.glyphs.border)
            .border_style(if hot {
                skin.palette.style(Role::Accent)
            } else {
                skin.palette.dim()
            });
        let inner = block.inner(boxes[i]);
        block.render(boxes[i], buf);
        if inner.width == 0 {
            continue;
        }
        let w = usize::from(inner.width);
        // The glyph+name line takes the accent when focused — the chip, this
        // page's focus idiom — and stays bold text otherwise.
        let mut name_line = centered(
            &format!("{} {name}", g.cat[i]),
            w,
            skin.palette.bold(Role::Text),
            skin,
        );
        if hot {
            name_line = banded(name_line, skin.palette.chip(Role::Accent));
        }
        set_row(inner, 0, buf, name_line);
        set_row(
            inner,
            1,
            buf,
            centered(
                &format!("{} memories", v.category_counts[i]),
                w,
                skin.palette.dim(),
                skin,
            ),
        );
    }
}

fn render_query(area: Rect, buf: &mut Buffer, v: &MemoryView, skin: &Skin) {
    let hot = matches!(v.focus, Some(Focus::QueryBox));
    let mut border = skin.palette.style(Role::Accent);
    if hot {
        border = border.add_modifier(Modifier::BOLD);
    }
    let block = Block::bordered()
        .border_set(skin.glyphs.border)
        .border_style(border);
    let inner = block.inner(area);
    block.render(area, buf);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let prefix = Span::styled(
        "> ".to_string(),
        if hot {
            skin.palette.chip(Role::Accent)
        } else {
            skin.palette.bold(Role::Accent)
        },
    );
    // The add flow borrows the box: a mode label names the flow and the
    // category, Tab cycles it, and the typed text becomes the memory.
    let mut left = vec![prefix];
    if let Some(cat) = v.adding {
        left.push(Span::styled(
            honest(
                &format!(
                    "New memory (category: {} — Tab cycles): ",
                    CATEGORIES[cat.min(5)].to_lowercase()
                ),
                skin.glyphs == ASCII,
            ),
            skin.palette.dim(),
        ));
        left.push(Span::styled(
            v.query.clone(),
            skin.palette.style(Role::Text),
        ));
    } else if v.query.is_empty() {
        left.push(Span::styled(PLACEHOLDER.to_string(), skin.palette.dim()));
    } else {
        left.push(Span::styled(
            v.query.clone(),
            skin.palette.style(Role::Text),
        ));
    }
    let line = lr(
        left,
        vec![Span::styled(
            "[send: Enter]".to_string(),
            skin.palette.dim(),
        )],
        usize::from(inner.width),
        skin,
    );
    buf.set_line(inner.x, inner.y, &line, inner.width);
}

// -- span carpentry ----------------------------------------------------------

/// A header affordance: accent normally, the chip when it is the focus.
fn affordance_span(text: String, hot: bool, skin: &Skin) -> Span<'static> {
    Span::styled(
        text,
        if hot {
            skin.palette.chip(Role::Accent)
        } else {
            skin.palette.style(Role::Accent)
        },
    )
}

/// Left spans, a gap, right spans, at exactly `w` columns. The right side
/// survives and the left truncates — the sidebar's trailing-column ruling.
fn lr(left: Vec<Span<'static>>, right: Vec<Span<'static>>, w: usize, skin: &Skin) -> Line<'static> {
    let rw: usize = right.iter().map(|s| cols(&s.content)).sum();
    let budget = w.saturating_sub(rw + 1);
    let mut out: Vec<Span<'static>> = Vec::new();
    let mut used = 0usize;
    for s in left {
        let cw = cols(&s.content);
        if used + cw <= budget {
            used += cw;
            out.push(s);
        } else {
            let room = budget.saturating_sub(used);
            if room > 0 {
                let t = clip(&s.content, room, skin);
                used += cols(&t);
                out.push(Span::styled(t, s.style));
            }
            break;
        }
    }
    out.push(Span::raw(" ".repeat(w.saturating_sub(used + rw))));
    out.extend(right);
    Line::from(out)
}

/// The sidebar's band: one style across every span, padding included.
fn banded(line: Line<'static>, style: Style) -> Line<'static> {
    Line::from(
        line.spans
            .into_iter()
            .map(|s| Span::styled(s.content, style))
            .collect::<Vec<_>>(),
    )
}

/// A dim honest empty state, one line.
fn empty_line(text: &str, w: usize, skin: &Skin) -> Line<'static> {
    let ascii = skin.glyphs == ASCII;
    Line::from(Span::styled(
        fit(&honest(text, ascii), w, skin.glyphs.ellipsis),
        skin.palette.dim(),
    ))
}

/// `text` centered in `w` columns.
fn centered(text: &str, w: usize, style: Style, skin: &Skin) -> Line<'static> {
    let t = clip(text, w, skin);
    let pad = w.saturating_sub(cols(&t)) / 2;
    Line::from(vec![Span::raw(" ".repeat(pad)), Span::styled(t, style)])
}

/// Paint one content row, clipped to the content area.
fn set_row(content: Rect, i: u16, buf: &mut Buffer, line: Line<'static>) {
    if i < content.height {
        buf.set_line(content.x, content.y + i, &line, content.width);
    }
}

/// [`fit`], made total for budgets narrower than the ellipsis — the settings
/// screen's `clip`, duplicated for the same reason it duplicated the
/// sidebar's: the original is private to its module.
fn clip(text: &str, budget: usize, skin: &Skin) -> String {
    if cols(text) <= budget {
        return text.to_string();
    }
    if budget < cols(skin.glyphs.ellipsis) {
        return ".".repeat(budget);
    }
    fit(text, budget, skin.glyphs.ellipsis)
}

// endregion: Rendering

#[cfg(test)]
mod tests {
    use super::super::palette::{Level, Palette};
    use super::super::render::UNICODE;
    use super::*;
    use ratatui::style::Color;

    fn skin() -> Skin {
        Skin::new(Palette::new(Level::Truecolor), UNICODE)
    }

    fn pin_row(slug: &str, title: &str) -> MemRow {
        MemRow {
            slug: slug.into(),
            title: title.into(),
            category: 2,
            created: "2026-08-26".into(),
            pinned: true,
        }
    }

    /// The mock's sample data, in full. This is what the page looks like once
    /// the wiki store and its index exist; the fidelity is pinned now.
    fn populated() -> MemoryView {
        MemoryView {
            version: "v0.6.3".into(),
            recent: vec![
                Recent {
                    source: Source::Doc,
                    text: "Prefers concise, bullet-point summaries".into(),
                    time: "10:15 AM".into(),
                    slug: "prefers-concise".into(),
                },
                Recent {
                    source: Source::Chat,
                    text: "Working on the Emma memory page".into(),
                    time: "10:02 AM".into(),
                    slug: "emma-memory-page".into(),
                },
                Recent {
                    source: Source::Doc,
                    text: "Uses ratatui for the terminal UI".into(),
                    time: "9:48 AM".into(),
                    slug: "uses-ratatui".into(),
                },
                Recent {
                    source: Source::Chat,
                    text: "Ollama runs locally on port 11434".into(),
                    time: "9:31 AM".into(),
                    slug: "ollama-port".into(),
                },
                Recent {
                    source: Source::Doc,
                    text: "Repo lives at ~/Projects/emma".into(),
                    time: "9:12 AM".into(),
                    slug: "repo-path".into(),
                },
            ],
            pinned: vec![
                pin_row("name-alan", "Name: Alan"),
                pin_row("timezone", "Timezone: US Eastern"),
                pin_row("prefers-rust", "Prefers Rust for systems work"),
                pin_row("reviews-noon", "Reviews land before noon"),
            ],
            convo_tokens: "~2.1k".into(),
            convo_messages: 18,
            transcript: vec![
                (
                    Speaker::Emma,
                    "I noted your preference for bullet summaries.".into(),
                ),
                (Speaker::You, "Also remember the deploy window.".into()),
                (Speaker::Emma, "Deploy window saved: Fridays, 2 PM.".into()),
                (Speaker::You, "Pin the timezone fact too.".into()),
                (Speaker::Emma, "Pinned: Timezone: US Eastern.".into()),
            ],
            older_archived: true,
            index: Some(IndexView {
                healthy: true,
                last_updated: "2 min ago".into(),
                indexed_tokens: "48.2k".into(),
                top_score_avg: "0.82".into(),
                latency: "12 ms".into(),
                coverage_pct: 92,
            }),
            total_memories: 128,
            embedding_model: "all-MiniLM-L6-v2".into(),
            category_counts: [24, 18, 42, 9, 13, 22],
            auto_save: true,
            query: String::new(),
            focus: None,
            notice: None,
            ..MemoryView::default()
        }
    }

    /// Today's truth: no store, no index, nothing to show but the chrome.
    fn empty() -> MemoryView {
        MemoryView {
            version: "v0.6.3".into(),
            embedding_model: "index-first (none)".into(),
            ..MemoryView::default()
        }
    }

    fn buffer(v: &MemoryView, w: u16, h: u16) -> Buffer {
        let area = Rect::new(0, 0, w, h);
        let mut buf = Buffer::empty(area);
        render(area, &mut buf, v, &skin());
        buf
    }

    fn lines(buf: &Buffer) -> Vec<String> {
        // A double-width glyph (the tip row's bulb) owns two cells; the
        // continuation cell reads back as a space and would double-count the
        // column, so it is skipped.
        let a = buf.area();
        (0..a.height)
            .map(|y| {
                let mut out = String::new();
                let mut x = 0;
                while x < a.width {
                    let sym = buf[(x, y)].symbol();
                    out.push_str(sym);
                    x += (cols(sym) as u16).max(1);
                }
                out.trim_end().to_string()
            })
            .collect()
    }

    fn draw(v: &MemoryView, w: u16, h: u16) -> Vec<String> {
        lines(&buffer(v, w, h))
    }

    fn row_with<'a>(rows: &'a [String], needle: &str) -> &'a String {
        rows.iter()
            .find(|r| r.contains(needle))
            .unwrap_or_else(|| panic!("{needle:?} not rendered"))
    }

    /// The buffer (x, y) of `needle`'s first cell, by display columns.
    fn locate(rows: &[String], needle: &str) -> (u16, u16) {
        for (y, r) in rows.iter().enumerate() {
            if let Some(i) = r.find(needle) {
                return (cols(&r[..i]) as u16, y as u16);
            }
        }
        panic!("{needle:?} not rendered");
    }

    /// `right` ends flush against the next card border (or the row's end) —
    /// the right-aligned column the mock draws everywhere.
    fn flush_right_of(row: &str, right: &str) {
        let i = row
            .find(right)
            .unwrap_or_else(|| panic!("{right:?} missing in {row:?}"));
        let after = &row[i + right.len()..];
        assert!(
            after.is_empty() || after.starts_with('│'),
            "{right:?} is not flush right: {row:?}"
        );
    }

    fn accent() -> Color {
        skin().palette.color(Role::Accent)
    }

    // -- the head and the action bar ---------------------------------------

    #[test]
    fn the_head_is_version_title_subtitle_rule() {
        let rows = draw(&populated(), 161, 75);
        assert!(
            rows[0].ends_with("v0.6.3"),
            "version not in the corner: {:?}",
            rows[0]
        );
        assert_eq!(rows[1], "Memory");
        assert_eq!(rows[2], SUBTITLE);
        assert!(
            rows[3].chars().all(|c| c == '─') && !rows[3].is_empty(),
            "rule missing"
        );
    }

    #[test]
    fn the_action_bar_is_the_mocks_exactly() {
        let rows = draw(&populated(), 161, 75);
        let bar = row_with(&rows, "[a] Add memory");
        assert_eq!(
            bar.trim_end(),
            "[a] Add memory   [s] Search   [p] Pin   [u] Unpin   [r] Reindex   [x] Archive   [R] Refresh   [?] Help"
        );
    }

    // -- card headers and their right affordances ---------------------------

    #[test]
    fn card_headers_carry_the_mocks_right_affordances() {
        let rows = draw(&populated(), 161, 75);
        let recent = row_with(&rows, "RECENT MEMORIES");
        assert!(
            recent.contains("View all →"),
            "View all affordance missing: {recent:?}"
        );
        let pinned = row_with(&rows, "PINNED FACTS (4)");
        assert!(
            pinned.contains("Manage →"),
            "Manage affordance missing: {pinned:?}"
        );
        assert_eq!(recent, pinned, "top pair does not share the header band");
        let convo = row_with(&rows, "CONVERSATION MEMORY");
        assert!(
            convo.contains("Tokens: ~2.1k   Messages: 18"),
            "conversation counts missing: {convo:?}"
        );
        assert!(
            convo.contains("RETRIEVAL STATUS"),
            "mid pair does not share the band"
        );
        row_with(&rows, "MEMORY CATEGORIES");
    }

    #[test]
    fn left_and_right_cards_sit_in_their_columns() {
        let rows = draw(&populated(), 161, 75);
        let mid = 161 / 2;
        for (needle, left) in [
            ("RECENT MEMORIES", true),
            ("PINNED FACTS (4)", false),
            ("CONVERSATION MEMORY", true),
            ("RETRIEVAL STATUS", false),
        ] {
            let x = rows
                .iter()
                .find_map(|r| r.find(needle))
                .unwrap_or_else(|| panic!("{needle} not rendered"));
            assert_eq!(x < mid, left, "{needle} in the wrong column (x={x})");
        }
    }

    // -- populated rendering, per mock --------------------------------------

    #[test]
    fn recent_rows_are_glyph_text_and_right_aligned_time() {
        let rows = draw(&populated(), 161, 75);
        for (text, time) in [
            ("Prefers concise, bullet-point summaries", "10:15 AM"),
            ("Ollama runs locally on port 11434", "9:31 AM"),
        ] {
            let row = row_with(&rows, text);
            assert!(row.contains(time), "time missing beside {text:?}: {row:?}");
            let inner = row.trim_start_matches(['│', ' ']);
            assert!(
                inner.starts_with('▤') || inner.starts_with('◆'),
                "no source glyph before {text:?}: {row:?}"
            );
        }
    }

    #[test]
    fn pinned_rows_wear_the_pin_and_the_trailing_dots_inside_an_inner_border() {
        let rows = draw(&populated(), 161, 75);
        let row = row_with(&rows, "Timezone: US Eastern");
        assert!(row.contains('⚑'), "pin glyph missing: {row:?}");
        assert!(row.contains("···"), "trailing dots missing: {row:?}");
        // The pin row sits inside the card's own inner border: two `│` on
        // each side of it (outer card + inner box).
        let before = row[..row.find('⚑').unwrap()].matches('│').count();
        assert!(before >= 2, "no inner border around the pin rows: {row:?}");
    }

    #[test]
    fn the_transcript_names_its_speakers_and_the_footer_is_the_mocks() {
        let rows = draw(&populated(), 161, 75);
        row_with(&rows, "I noted your preference for bullet summaries.");
        row_with(&rows, "Also remember the deploy window.");

        // **The words start in one column, whoever is speaking.** This used to
        // assert the two rows verbatim, two spaces after each name -- which
        // encoded the bug: `Emma` is four columns and `You` is three, so every
        // other line's text sat one column left of its neighbour's. The
        // literal passed because it was copied from the output.
        //
        // Asserted as a property so a third speaker, or a translated label,
        // cannot quietly re-ragged the edge.
        let (emma_x, _) = locate(&rows, "I noted your preference");
        let (you_x, _) = locate(&rows, "Also remember the deploy");
        assert_eq!(
            emma_x, you_x,
            "the transcript's text does not start in one column: Emma at {emma_x}, You at {you_x}"
        );

        let footer = row_with(&rows, "Older messages archived");
        assert!(
            footer.contains("View full history →"),
            "history affordance missing: {footer:?}"
        );
    }

    #[test]
    fn retrieval_status_rows_carry_the_mocks_labels_and_flush_values() {
        let rows = draw(&populated(), 161, 75);
        let status = row_with(&rows, "Index status");
        assert!(
            status.contains("✓ Healthy"),
            "state glyph missing: {status:?}"
        );
        flush_right_of(status, "✓ Healthy");
        for (label, value) in [
            ("Last updated", "2 min ago"),
            ("Total memories", "128"),
            ("Indexed tokens", "48.2k"),
            ("Embedding model", "all-MiniLM-L6-v2"),
            ("Top score average", "0.82"),
            ("Retrieval latency", "12 ms"),
        ] {
            let row = row_with(&rows, label);
            assert!(
                row.contains(value),
                "{value:?} missing beside {label:?}: {row:?}"
            );
            flush_right_of(row, value);
        }
        let cov = row_with(&rows, "Index coverage");
        assert!(cov.contains('█'), "gauge missing: {cov:?}");
        flush_right_of(cov, "92%");
    }

    #[test]
    fn the_gauge_is_proportional() {
        let count = |pct: u8| {
            let mut v = populated();
            v.index.as_mut().unwrap().coverage_pct = pct;
            let rows = draw(&v, 161, 75);
            let cov = row_with(&rows, "Index coverage").clone();
            (cov.matches('█').count(), cov.matches('░').count())
        };
        let (f0, e0) = count(0);
        assert_eq!(f0, 0, "0% shows filled segments");
        let (f100, e100) = count(100);
        assert_eq!(e100, 0, "100% shows empty segments");
        let (f50, e50) = count(50);
        assert!(f50 > 0 && e50 > 0, "50% is not mixed");
        assert!(f50.abs_diff(e50) <= 1, "50% is not half: {f50} vs {e50}");
        assert_eq!(f0 + e0, f100 + e100, "gauge width moved with the value");
        let (f20, _) = count(20);
        let (f80, _) = count(80);
        assert!(f80 > f20, "gauge is not monotonic");
    }

    #[test]
    fn six_category_boxes_side_by_side_with_real_counts() {
        let rows = draw(&populated(), 161, 75);
        let name_row = row_with(&rows, "Preferences");
        let mut last = 0;
        for name in CATEGORIES {
            let x = name_row
                .find(name)
                .unwrap_or_else(|| panic!("{name} missing"));
            assert!(x >= last, "{name} out of order");
            last = x + name.len();
        }
        let count_row = row_with(&rows, "24 memories");
        for n in ["24", "18", "42", "9", "13", "22"] {
            assert!(
                count_row.contains(&format!("{n} memories")),
                "{n} memories missing: {count_row:?}"
            );
        }
        let tops = rows.iter().filter(|r| r.matches('╭').count() == 6).count();
        assert!(tops >= 1, "no row of six mini-box tops");
    }

    #[test]
    fn the_tip_row_and_the_query_box_are_the_mocks() {
        let rows = draw(&populated(), 161, 75);
        let tip = row_with(&rows, TIP);
        assert!(tip.contains("💡"), "bulb missing: {tip:?}");
        assert!(
            tip.contains("● Auto-save enabled"),
            "auto-save dot missing: {tip:?}"
        );
        assert!(
            tip.trim_end().ends_with("Auto-save enabled"),
            "not right-aligned: {tip:?}"
        );
        let q = row_with(&rows, PLACEHOLDER);
        let inner = q.trim_start_matches(['│', ' ']);
        assert!(inner.starts_with("> "), "prompt prefix missing: {q:?}");
        assert!(q.contains("[send: Enter]"), "send hint missing: {q:?}");
    }

    // -- the bottom anchor (owner defect, 2026-08-26) ------------------------

    /// The owner's screenshot: a short terminal lost the query box because it
    /// rendered last, top-down. The fix anchors it (and the tip row) to the
    /// bottom, like the chat input box, at every height a person would use.
    #[test]
    fn the_query_box_is_bottom_anchored_at_short_heights() {
        for v in [populated(), empty()] {
            for h in [20u16, 30, 40] {
                let rows = draw(&v, 161, h);
                let last = usize::from(h) - 1;
                assert!(
                    rows[last].starts_with('╰'),
                    "no query box bottom border at {h}: {:?}",
                    rows[last]
                );
                assert!(
                    rows[last - 2].starts_with('╭'),
                    "no query box top border at {h}: {:?}",
                    rows[last - 2]
                );
                let q = &rows[last - 1];
                assert!(
                    q.contains("> ") && q.contains("[send: Enter]"),
                    "query row missing at {h}: {q:?}"
                );
                assert!(
                    rows[last - 3].contains(TIP),
                    "tip row not above the query box at {h}: {:?}",
                    rows[last - 3]
                );
                // Never overlapped: the row above the box's top border belongs
                // to the tip row, not to a card that ran long.
                assert!(
                    !rows[last - 3].contains('│'),
                    "a card overlaps the tip row at {h}: {:?}",
                    rows[last - 3]
                );
            }
        }
    }

    /// A card too short for its rows keeps the header and says what it hid,
    /// instead of silently clipping: `… N more`.
    #[test]
    fn short_windows_compress_cards_honestly() {
        let rows = draw(&populated(), 161, 30);
        // Every card header survives the squeeze at 30 rows.
        for header in [
            "RECENT MEMORIES",
            "PINNED FACTS (4)",
            "CONVERSATION MEMORY",
            "RETRIEVAL STATUS",
            "MEMORY CATEGORIES",
        ] {
            row_with(&rows, header);
        }
        let all = rows.join("\n");
        assert!(
            all.contains("… ") && all.contains(" more"),
            "no honest truncation marker at 30 rows"
        );
    }

    // -- the taller category boxes (owner defect, 2026-08-26) ----------------

    /// Glyph and name share one bold line, the count sits under it, and a row
    /// of air fills the third inner row — presence, since size is one cell.
    #[test]
    fn category_boxes_put_glyph_and_name_together_over_the_count() {
        let rows = draw(&populated(), 161, 75);
        let (_, y) = locate(&rows, "Preferences");
        let name_row = &rows[usize::from(y)];
        assert!(
            name_row.contains("⚙ Preferences"),
            "glyph and name not on one line: {name_row:?}"
        );
        for glyph_name in [
            "▦ Projects",
            "◇ Facts",
            "↻ Workflows",
            "☺ People",
            "§ References",
        ] {
            assert!(
                name_row.contains(glyph_name),
                "{glyph_name:?} missing: {name_row:?}"
            );
        }
        let count_row = &rows[usize::from(y) + 1];
        assert!(
            count_row.contains("24 memories"),
            "count not directly under the name: {count_row:?}"
        );
        // The box is five rows: top border, name, count, air, bottom border.
        assert!(
            rows[usize::from(y) - 1].matches('╭').count() >= 6,
            "box tops missing"
        );
        assert!(
            rows[usize::from(y) + 3].matches('╰').count() >= 6,
            "box is not 3 rows inner"
        );
        let air = &rows[usize::from(y) + 2];
        assert!(
            !air.chars().any(|c| c.is_alphanumeric()),
            "no row of air inside the box: {air:?}"
        );
    }

    /// Too little height for full boxes renders the honest one-line fallback,
    /// never a squashed half-box.
    #[test]
    fn squashed_category_boxes_are_never_drawn() {
        for h in [20u16, 24, 26] {
            let rows = draw(&populated(), 161, h);
            let tops = rows.iter().filter(|r| r.matches('╭').count() == 6).count();
            if tops == 0 {
                continue; // the card fell back or fell off; either is honest
            }
            // If box tops render, the full 5-row geometry must too.
            let all = rows.join("\n");
            assert!(all.contains("⚙ Preferences"), "half-drawn boxes at {h}");
            assert!(all.contains("24 memories"), "boxes without counts at {h}");
        }
    }

    // -- the focus model, render-only ---------------------------------------

    #[test]
    fn a_focused_row_wears_the_band_and_only_that_row() {
        let mut v = populated();
        v.focus = Some(Focus::RecentRow(1));
        let buf = buffer(&v, 161, 75);
        let rows = lines(&buf);
        let (x1, y1) = locate(&rows, "Working on the Emma memory page");
        assert_eq!(
            buf[(x1, y1)].style().bg,
            Some(accent()),
            "no band on the focused row"
        );
        let (x0, y0) = locate(&rows, "Prefers concise, bullet-point summaries");
        assert_ne!(
            buf[(x0, y0)].style().bg,
            Some(accent()),
            "band leaked to another row"
        );
        // The owning card's border takes the accent...
        assert_eq!(
            buf[(0, y1)].style().fg,
            Some(accent()),
            "focused card border not accent"
        );
        // ...and an unfocused card's stays dim (the pinned card's right edge).
        let (_, yp) = locate(&rows, "Timezone: US Eastern");
        let pin_row = &rows[usize::from(yp)];
        let bi = pin_row.find("Timezone").expect("pinned text missing");
        let bidx = pin_row[..bi].rfind('│').expect("pinned border missing");
        let pin_border = cols(&pin_row[..bidx]) as u16;
        assert_ne!(
            buf[(pin_border, yp)].style().fg,
            Some(accent()),
            "accent leaked to another card"
        );
    }

    #[test]
    fn focused_dots_and_affordances_light_up_accent_bright() {
        let mut v = populated();
        v.focus = Some(Focus::PinnedDots(1));
        let buf = buffer(&v, 161, 75);
        let rows = lines(&buf);
        let (_, y) = locate(&rows, "Timezone: US Eastern");
        let row = &rows[usize::from(y)];
        let dx = cols(&row[..row.find("···").expect("dots missing")]) as u16;
        assert_eq!(
            buf[(dx, y)].style().bg,
            Some(accent()),
            "focused dots not lit"
        );
        // An unfocused row's dots stay dim, no band.
        let (_, y0) = locate(&rows, "Name: Alan");
        let row0 = &rows[usize::from(y0)];
        let dx0 = cols(&row0[..row0.find("···").expect("dots missing")]) as u16;
        assert_ne!(
            buf[(dx0, y0)].style().bg,
            Some(accent()),
            "band leaked to unfocused dots"
        );

        let mut v = populated();
        v.focus = Some(Focus::RecentViewAll);
        let buf = buffer(&v, 161, 75);
        let rows = lines(&buf);
        let (ax, ay) = locate(&rows, "View all →");
        assert_eq!(
            buf[(ax, ay)].style().bg,
            Some(accent()),
            "focused View all not lit"
        );

        let mut v = populated();
        v.focus = Some(Focus::QueryBox);
        let buf = buffer(&v, 161, 75);
        let rows = lines(&buf);
        let (qx, qy) = locate(&rows, "> ");
        assert_eq!(
            buf[(qx, qy)].style().bg,
            Some(accent()),
            "focused query prompt not lit"
        );
    }

    #[test]
    fn a_focused_category_box_takes_the_accent() {
        let mut v = populated();
        v.focus = Some(Focus::CategoryBox(3));
        let buf = buffer(&v, 161, 75);
        let rows = lines(&buf);
        let (wx, wy) = locate(&rows, "Workflows");
        assert_eq!(
            buf[(wx, wy)].style().bg,
            Some(accent()),
            "focused mini-box name not lit"
        );
        let (px, py) = locate(&rows, "Projects");
        assert_ne!(
            buf[(px, py)].style().bg,
            Some(accent()),
            "accent leaked to another mini-box"
        );
    }

    // -- the honest empty page ----------------------------------------------

    #[test]
    fn the_empty_page_keeps_the_chrome_and_tells_the_truth() {
        let rows = draw(&empty(), 161, 75);
        row_with(&rows, "[a] Add memory");
        row_with(&rows, EMPTY_RECENT);
        row_with(&rows, EMPTY_PINNED);
        row_with(&rows, EMPTY_CONVO);
        row_with(&rows, "PINNED FACTS (0)");
        let convo = row_with(&rows, "CONVERSATION MEMORY");
        assert!(
            convo.contains("Tokens: 0   Messages: 0"),
            "counts are not real zeros: {convo:?}"
        );
        let status = row_with(&rows, "Index status");
        assert!(status.contains(NO_INDEX), "status not honest: {status:?}");
        let total = row_with(&rows, "Total memories");
        flush_right_of(total, "0");
        let model = row_with(&rows, "Embedding model");
        assert!(
            model.contains("index-first (none)"),
            "model row not verbatim: {model:?}"
        );
        let all = rows.join("\n");
        assert_eq!(
            all.matches("0 memories").count(),
            6,
            "category zeros missing"
        );
        let cov = row_with(&rows, "Index coverage");
        assert!(!cov.contains('█'), "empty index claims coverage: {cov:?}");
        flush_right_of(cov, "0%");
    }

    #[test]
    fn the_empty_page_makes_no_false_claims() {
        let all = draw(&empty(), 161, 75).join("\n");
        assert!(
            !all.contains("Older messages archived"),
            "archival claimed with no archive"
        );
        assert!(
            !all.contains("Auto-save enabled"),
            "auto-save claimed with no store"
        );
        assert!(!all.contains("Healthy"), "health claimed with no index");
        for sample in ["all-MiniLM-L6-v2", "2 min ago", "~2.1k"] {
            assert!(!all.contains(sample), "sample data leaked: {sample}");
        }
    }

    // -- the key seam (M5): two scopes, pure ---------------------------------

    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::from(code)
    }

    fn ch(c: char) -> KeyEvent {
        press(KeyCode::Char(c))
    }

    fn type_text(v: &mut MemoryView, text: &str) {
        for c in text.chars() {
            assert_eq!(handle_key(v, ch(c)), MemoryAction::FocusChanged);
        }
    }

    #[test]
    fn every_action_bar_key_acts_when_the_query_box_is_not_focused() {
        let mut v = populated();
        v.focus = Some(Focus::RecentRow(1));
        assert_eq!(
            handle_key(&mut v, ch('p')),
            MemoryAction::Pin("emma-memory-page".into())
        );
        assert_eq!(
            handle_key(&mut v, ch('u')),
            MemoryAction::Unpin("emma-memory-page".into())
        );
        assert_eq!(
            handle_key(&mut v, ch('x')),
            MemoryAction::Archive("emma-memory-page".into())
        );
        assert_eq!(handle_key(&mut v, ch('r')), MemoryAction::Reindex);
        assert_eq!(handle_key(&mut v, ch('R')), MemoryAction::Refresh);
        assert_eq!(handle_key(&mut v, ch('s')), MemoryAction::FocusChanged);
        assert_eq!(
            v.notice.as_deref(),
            Some(NOTICE_M2),
            "search key not honest"
        );
        assert_eq!(handle_key(&mut v, ch('?')), MemoryAction::Help);
        assert_eq!(v.notice.as_deref(), Some(HELP_MAIN));
        assert_eq!(handle_key(&mut v, ch('?')), MemoryAction::Help);
        assert_eq!(v.notice, None, "help notice does not toggle off");
        assert_eq!(handle_key(&mut v, ch('a')), MemoryAction::FocusChanged);
        assert_eq!(
            v.adding,
            Some(ADD_CATEGORY_START),
            "add flow starts at Facts"
        );
        assert_eq!(
            v.focus,
            Some(Focus::QueryBox),
            "add flow does not take the box"
        );
    }

    #[test]
    fn a_pinned_row_selection_acts_on_the_pinned_slug() {
        let mut v = populated();
        v.focus = Some(Focus::PinnedDots(2));
        assert_eq!(
            handle_key(&mut v, ch('u')),
            MemoryAction::Unpin("prefers-rust".into())
        );
        assert_eq!(
            handle_key(&mut v, ch('x')),
            MemoryAction::Archive("prefers-rust".into())
        );
    }

    #[test]
    fn action_keys_without_a_selection_ask_for_one() {
        let mut v = populated();
        v.focus = None;
        for key in ['p', 'u', 'x'] {
            v.notice = None;
            assert_eq!(handle_key(&mut v, ch(key)), MemoryAction::FocusChanged);
            assert_eq!(
                v.notice.as_deref(),
                Some(NOTICE_NO_ROW),
                "[{key}] acted with no row"
            );
        }
    }

    #[test]
    fn printable_keys_type_into_a_focused_query_box_instead_of_acting() {
        let mut v = populated();
        v.focus = Some(Focus::QueryBox);
        type_text(&mut v, "pux");
        assert_eq!(v.query, "pux", "action keys did not type");
        assert_eq!(
            handle_key(&mut v, press(KeyCode::Backspace)),
            MemoryAction::FocusChanged
        );
        assert_eq!(v.query, "pu");
        assert_eq!(
            handle_key(&mut v, press(KeyCode::Enter)),
            MemoryAction::Query("pu".into())
        );
        assert_eq!(
            handle_key(&mut v, press(KeyCode::Esc)),
            MemoryAction::FocusChanged
        );
        assert_eq!(v.focus, None, "Esc does not leave the box");
        assert_eq!(v.query, "pu", "a plain query loses its text on Esc");
    }

    #[test]
    fn enter_on_an_empty_query_does_nothing() {
        let mut v = populated();
        v.focus = Some(Focus::QueryBox);
        assert_eq!(
            handle_key(&mut v, press(KeyCode::Enter)),
            MemoryAction::None
        );
    }

    #[test]
    fn the_add_flow_cycles_categories_with_tab_and_creates_on_enter() {
        let mut v = populated();
        handle_key(&mut v, ch('a'));
        assert_eq!(
            v.adding,
            Some(2),
            "starts at facts, the owner's example label"
        );
        for expect in [3, 4, 5, 0, 1, 2] {
            assert_eq!(
                handle_key(&mut v, press(KeyCode::Tab)),
                MemoryAction::FocusChanged
            );
            assert_eq!(
                v.adding,
                Some(expect),
                "Tab does not cycle the six categories"
            );
        }
        v.adding = Some(1); // park it on Projects and create there
        type_text(&mut v, "Deploy window");
        assert_eq!(
            handle_key(&mut v, press(KeyCode::Enter)),
            MemoryAction::Add {
                title: "Deploy window".into(),
                category: 1
            }
        );
        assert_eq!(v.adding, None, "add flow does not end on Enter");
        assert_eq!(v.query, "", "the typed title lingers in the box");
        assert_eq!(v.focus, None);
    }

    #[test]
    fn esc_cancels_the_add_flow_whole() {
        let mut v = populated();
        handle_key(&mut v, ch('a'));
        type_text(&mut v, "half a thought");
        assert_eq!(
            handle_key(&mut v, press(KeyCode::Esc)),
            MemoryAction::FocusChanged
        );
        assert_eq!(v.adding, None);
        assert_eq!(v.query, "", "a cancelled add keeps its text");
        assert_eq!(v.focus, None);
    }

    #[test]
    fn tab_cycles_the_cards_in_the_recorded_order() {
        let mut v = populated();
        let order = [
            Focus::RecentRow(0),
            Focus::PinnedDots(0),
            Focus::ConvoHistory,
            Focus::Retrieval,
            Focus::CategoryBox(0),
            Focus::QueryBox,
            Focus::RecentRow(0), // and around again, out of the query box
        ];
        for expect in order {
            assert_eq!(
                handle_key(&mut v, press(KeyCode::Tab)),
                MemoryAction::FocusChanged
            );
            assert_eq!(v.focus, Some(expect), "Tab order diverges at {expect:?}");
        }
        // Empty cards enter at their header affordance instead of a row.
        let mut v = empty();
        handle_key(&mut v, press(KeyCode::Tab));
        assert_eq!(v.focus, Some(Focus::RecentViewAll));
        handle_key(&mut v, press(KeyCode::Tab));
        assert_eq!(v.focus, Some(Focus::PinnedManage));
    }

    #[test]
    fn arrows_move_through_rows_and_the_header_affordance() {
        let mut v = populated();
        v.focus = Some(Focus::RecentRow(0));
        handle_key(&mut v, press(KeyCode::Down));
        assert_eq!(v.focus, Some(Focus::RecentRow(1)));
        handle_key(&mut v, press(KeyCode::Up));
        handle_key(&mut v, press(KeyCode::Up));
        assert_eq!(
            v.focus,
            Some(Focus::RecentViewAll),
            "Up from row 0 is the affordance"
        );
        handle_key(&mut v, press(KeyCode::Down));
        assert_eq!(v.focus, Some(Focus::RecentRow(0)));
        // Clamped at the end.
        v.focus = Some(Focus::RecentRow(4));
        assert_eq!(handle_key(&mut v, press(KeyCode::Down)), MemoryAction::None);
        // Category boxes step sideways.
        v.focus = Some(Focus::CategoryBox(0));
        handle_key(&mut v, press(KeyCode::Down));
        assert_eq!(v.focus, Some(Focus::CategoryBox(1)));
        handle_key(&mut v, press(KeyCode::Up));
        assert_eq!(v.focus, Some(Focus::CategoryBox(0)));
        assert_eq!(handle_key(&mut v, press(KeyCode::Up)), MemoryAction::None);
    }

    #[test]
    fn chords_and_releases_are_never_this_pages_keys() {
        let mut v = populated();
        v.focus = Some(Focus::RecentRow(0));
        let alt = KeyEvent::new(KeyCode::Char('m'), KeyModifiers::ALT);
        assert_eq!(handle_key(&mut v, alt), MemoryAction::None);
        let ctrl = KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL);
        assert_eq!(handle_key(&mut v, ctrl), MemoryAction::None);
        let mut release = ch('p');
        release.kind = ratatui::crossterm::event::KeyEventKind::Release;
        assert_eq!(handle_key(&mut v, release), MemoryAction::None);
        assert_eq!(
            v.focus,
            Some(Focus::RecentRow(0)),
            "a chord moved the focus"
        );
    }

    // -- the sub-views (owner addition, 2026-08-26) --------------------------

    fn many_rows() -> Vec<MemRow> {
        (0..30)
            .map(|i| MemRow {
                slug: format!("mem-{i:02}"),
                title: format!("Memory number {i:02}"),
                category: i % 6,
                created: "2026-08-26".into(),
                pinned: i % 5 == 0,
            })
            .collect()
    }

    #[test]
    fn enter_on_the_header_affordances_opens_the_sub_views_and_b_returns() {
        let mut v = populated();
        v.focus = Some(Focus::RecentViewAll);
        handle_key(&mut v, press(KeyCode::Enter));
        assert_eq!(v.mode, PageMode::AllMemories);
        handle_key(&mut v, ch('b'));
        assert_eq!(v.mode, PageMode::Main);
        v.focus = Some(Focus::PinnedManage);
        handle_key(&mut v, press(KeyCode::Enter));
        assert_eq!(v.mode, PageMode::ManagePinned);
        handle_key(&mut v, press(KeyCode::Esc));
        assert_eq!(v.mode, PageMode::Main);
        // [v] is the shortcut; it does not collide with the main key map.
        v.focus = None;
        handle_key(&mut v, ch('v'));
        assert_eq!(v.mode, PageMode::AllMemories);
    }

    #[test]
    fn the_filter_cycles_all_then_each_category_then_all() {
        let mut v = populated();
        v.all = many_rows();
        v.mode = PageMode::AllMemories;
        let mut seen = vec![v.all_filter];
        for _ in 0..7 {
            handle_key(&mut v, ch('f'));
            seen.push(v.all_filter);
        }
        assert_eq!(
            seen,
            vec![
                None,
                Some(0),
                Some(1),
                Some(2),
                Some(3),
                Some(4),
                Some(5),
                None
            ],
            "the filter cycle is not All -> the six -> All"
        );
        v.all_filter = Some(3);
        let shown = filtered_all(&v);
        assert!(
            shown.iter().all(|&i| v.all[i].category == 3),
            "filter leaks other categories"
        );
        assert_eq!(shown.len(), 5);
    }

    #[test]
    fn sub_view_selection_moves_and_the_action_keys_take_the_selected_slug() {
        let mut v = populated();
        v.all = many_rows();
        v.mode = PageMode::AllMemories;
        handle_key(&mut v, press(KeyCode::Down));
        handle_key(&mut v, press(KeyCode::Down));
        assert_eq!(v.all_selected, 2);
        assert_eq!(
            handle_key(&mut v, ch('x')),
            MemoryAction::Archive("mem-02".into())
        );
        assert_eq!(
            handle_key(&mut v, ch('p')),
            MemoryAction::Pin("mem-02".into())
        );
        // Under a filter the selection indexes the filtered list.
        handle_key(&mut v, ch('f')); // Preferences (category 0): 00,06,12,18,24
        handle_key(&mut v, press(KeyCode::Down));
        assert_eq!(
            handle_key(&mut v, ch('u')),
            MemoryAction::Unpin("mem-06".into())
        );
        // Manage pinned: the pinned list is the selection space.
        let mut v = populated();
        v.mode = PageMode::ManagePinned;
        handle_key(&mut v, press(KeyCode::Down));
        assert_eq!(
            handle_key(&mut v, ch('u')),
            MemoryAction::Unpin("timezone".into())
        );
        assert_eq!(
            handle_key(&mut v, ch('x')),
            MemoryAction::Archive("timezone".into())
        );
    }

    #[test]
    fn the_all_memories_view_lists_rows_with_filter_and_position() {
        let mut v = populated();
        v.all = many_rows();
        v.mode = PageMode::AllMemories;
        let rows = draw(&v, 161, 50);
        row_with(&rows, "All Memories");
        let bar = row_with(&rows, "[f] Filter");
        assert!(
            bar.contains("[b] Back"),
            "sub-view action bar wrong: {bar:?}"
        );
        assert!(
            !bar.contains("Add memory"),
            "main-page keys claimed on the sub-view: {bar:?}"
        );
        let header = row_with(&rows, "ALL MEMORIES");
        assert!(header.contains("Filter: All"), "no filter name: {header:?}");
        assert!(header.contains("1 of 30"), "no N of M: {header:?}");
        let first = row_with(&rows, "Memory number 00");
        assert!(
            first.contains("Preferences"),
            "no category column: {first:?}"
        );
        assert!(first.contains("2026-08-26"), "no created column: {first:?}");
        assert!(
            first.contains('⚑'),
            "pinned row missing its glyph: {first:?}"
        );
        let second = row_with(&rows, "Memory number 01");
        assert!(
            !second.contains('⚑'),
            "unpinned row claims a pin: {second:?}"
        );
    }

    #[test]
    fn the_all_view_scrolls_with_the_selection_and_says_n_of_m() {
        let mut v = populated();
        v.all = many_rows();
        v.mode = PageMode::AllMemories;
        v.all_selected = 24;
        let rows = draw(&v, 161, 24);
        let header = row_with(&rows, "ALL MEMORIES");
        assert!(header.contains("25 of 30"), "position wrong: {header:?}");
        row_with(&rows, "Memory number 24");
        assert!(
            !rows.iter().any(|r| r.contains("Memory number 00")),
            "the viewport did not scroll"
        );
    }

    #[test]
    fn the_all_view_filtered_header_and_empty_state_are_honest() {
        let mut v = populated();
        v.all = many_rows()
            .into_iter()
            .filter(|r| r.category != 3)
            .collect();
        v.mode = PageMode::AllMemories;
        v.all_filter = Some(3);
        let rows = draw(&v, 161, 40);
        let header = row_with(&rows, "ALL MEMORIES");
        assert!(
            header.contains("Filter: Workflows"),
            "filter not named: {header:?}"
        );
        assert!(
            header.contains("(0 of 25)"),
            "shown/total wrong: {header:?}"
        );
        row_with(&rows, "No Workflows memories — [f] cycles the filter");
    }

    #[test]
    fn the_manage_pinned_view_lists_and_its_empty_state_points_home() {
        let mut v = populated();
        v.mode = PageMode::ManagePinned;
        v.pin_selected = 1;
        let rows = draw(&v, 161, 40);
        row_with(&rows, "Manage Pinned");
        let header = row_with(&rows, "MANAGE PINNED (4)");
        assert!(header.contains("2 of 4"), "position wrong: {header:?}");
        let bar = row_with(&rows, "[u] Unpin");
        assert!(
            !bar.contains("[p] Pin"),
            "pin offered where everything is pinned: {bar:?}"
        );
        row_with(&rows, "Timezone: US Eastern");
        let mut v = populated();
        v.pinned.clear();
        v.mode = PageMode::ManagePinned;
        let rows = draw(&v, 161, 40);
        row_with(&rows, EMPTY_MANAGE);
    }

    #[test]
    fn the_add_flow_relabels_the_query_box() {
        let mut v = populated();
        handle_key(&mut v, ch('a'));
        type_text(&mut v, "Ship it");
        let rows = draw(&v, 161, 40);
        let q = row_with(&rows, "New memory (category: facts — Tab cycles):");
        assert!(
            q.contains("Ship it"),
            "typed text missing from the add box: {q:?}"
        );
        assert!(
            !q.contains(PLACEHOLDER),
            "placeholder over the add label: {q:?}"
        );
    }

    #[test]
    fn a_notice_takes_the_tip_rows_place_until_dismissed() {
        let mut v = populated();
        handle_key(&mut v, ch('s'));
        let rows = draw(&v, 161, 40);
        row_with(&rows, NOTICE_M2);
        assert!(
            !rows.iter().any(|r| r.contains(TIP)),
            "tip and notice rendered together"
        );
        handle_key(&mut v, press(KeyCode::Esc));
        let rows = draw(&v, 161, 40);
        row_with(&rows, TIP);
    }

    // -- invariants ----------------------------------------------------------

    #[test]
    fn no_row_is_ever_wider_than_the_area() {
        for v in [populated(), empty()] {
            for (w, h) in [
                (161, 75),
                (120, 60),
                (100, 40),
                (80, 30),
                (60, 20),
                (40, 12),
                (20, 8),
                (5, 3),
            ] {
                for row in draw(&v, w, h) {
                    assert!(
                        cols(&row) <= usize::from(w),
                        "row overruns at {w}x{h}: {row:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn the_ascii_skin_paints_no_multibyte_glyphs() {
        let area = Rect::new(0, 0, 120, 60);
        let mut buf = Buffer::empty(area);
        render(
            area,
            &mut buf,
            &populated(),
            &Skin::new(Palette::new(Level::Ansi16), ASCII),
        );
        for y in 0..60u16 {
            let row: String = (0..120u16)
                .map(|x| buf[(x, y)].symbol().to_string())
                .collect();
            assert!(row.is_ascii(), "non-ASCII under the ASCII skin: {row:?}");
        }
    }

    /// Eyeball dump: `cargo test -p emma the_populated_page -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn the_populated_page_at_161x75_for_eyeballing() {
        for row in draw(&populated(), 161, 75) {
            println!("{row}");
        }
    }
}
