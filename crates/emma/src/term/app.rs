//! The full-screen frame: the layout that owns the whole window, and the state
//! that survives between paints.
//!
//! This is stage 2 of `notes/design/tui-fullscreen.md` — the point of no
//! return. The alternate screen is entered in [`super::frame`]; what happens on
//! it is decided here. Everything in this file is pure over a [`Buffer`], so a
//! test can hold the layout still; the terminal, the locks and the entry/leave
//! bytes stay in `frame.rs`, which has already shipped two invisible-to-tests
//! defects and does not need a third file's worth of chances.
//!
//! # The regions
//!
//! Sidebar on the left (width from [`sidebar::width`], zero when collapsed),
//! status bar full-width at the bottom, and the main pane between them:
//! a header, a rule, the transcript, the input dock, and a one-row hint. The
//! proportions are the mockup's, measured from `notes/design/mockup-tui.png` itself
//! (re-measured 2026-08-13) — the prose description of that image was wrong in
//! five recorded places, and the design note's §1.1 sampled table, though
//! pixel-derived, still missed three facts the image shows: the panes do not
//! share edges (two columns of ground between sidebar and main pane), one
//! blank row separates the main pane from the status bar, and everything
//! floats one cell inside the window edge. [`regions`] carries each with the
//! measurement that decided it.
//!
//! # What owns what
//!
//! - The **transcript** is [`Transcript`] — the owned replacement for the
//!   terminal scrollback the alternate screen costs. Scroll offsets, the follow
//!   latch and the cap all live there; this file only routes keys to it and
//!   hands it a width.
//! - The **blank-row rule** moves with it: in the full-screen frame,
//!   [`Transcript`] is the one place separators are decided — every block
//!   pushed gets exactly one blank row from its neighbour. `separate()` on the
//!   inline path had to be a declaration because the terminal owned the rows;
//!   here the buffer owns them and the rule is structural. One consequence,
//!   named rather than hidden: each `write_lines` call is one block, so a tool
//!   start and its result are separated by a blank row where the inline path
//!   packed them tight. Coalescing runs of tool traffic into one block needs an
//!   append API on [`Transcript`], which is shared-owned and not this change's
//!   to grow — see the report.
//! - The **input dock** reuses [`View`]'s own renderers — the same input box,
//!   menu and approval panel the inline viewport drew, in a new place. The
//!   approval prompt *replaces* the input box region, exactly as it does
//!   inline: it is fixed chrome, and no amount of output can move it.
//! - The **sidebar** and **status bar** are the other agents' widgets; this
//!   file owns their state and their rectangle, never their cells.
//!
//! # The sidebar latch
//!
//! Collapse below [`sidebar::AUTO_COLLAPSE_COLS`] is automatic *until the user
//! has an opinion*: a user toggle latches in both directions, and a
//! user-expanded sidebar below the threshold is honoured. Same reasoning as
//! `frame.rs`'s `pinned` latch — distinguish where the system put it from
//! where the user put it — and the same shape: one bool for the posture, one
//! for whose posture it is.

use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Margin, Position, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Widget};

use super::palette::Role;
use super::render::{fit, Skin};
use super::transcript::{Cap, EntryKind, Transcript};
use super::view::View;
use super::{chat, sidebar, statusbar};

// region: The latch
// ---------------------------------------------------------------------------
// The latch
// ---------------------------------------------------------------------------

/// Whether the sidebar is collapsed, and whether that was a person's decision.
#[derive(Debug, Clone, Copy, Default)]
pub struct Latch {
    pub collapsed: bool,
    pub by_user: bool,
}

/// Is the sidebar hidden at this width?
///
/// The user's latch wins outright; below it, the width decides. An automatic
/// collapse un-collapses when the window widens past the threshold again — it
/// was never a choice — which falls out of `by_user` being false rather than
/// being a rule of its own.
pub fn hidden(total_cols: u16, latch: Latch) -> bool {
    if latch.by_user {
        latch.collapsed
    } else {
        total_cols < sidebar::AUTO_COLLAPSE_COLS
    }
}

// endregion: The latch

// region: The layout
// ---------------------------------------------------------------------------
// The layout
//
// Pure functions of (area, state) → rectangles, so every threshold in the
// design's §9 that stage 2 implements is a case a test can name. The ones
// stage 2 does not implement (gutter collapse, cell shedding) belong to the
// widgets that will own those cells.
// ---------------------------------------------------------------------------

/// Below this many rows the status bar loses its border: one row, not three.
pub const SLIM_STATUS_ROWS: u16 = 14;

/// Below this many rows the header is one line and the rule goes.
pub const SLIM_HEADER_ROWS: u16 = 12;

/// Below this many columns the main pane loses its border.
pub const UNBORDERED_COLS: u16 = 60;

/// Below this many rows the vertical float — the mockup's one blank row above
/// the panes and below the bar — is shed. Rows are the scarce axis: at 24 rows
/// two of them are a whole approval key row, and the mockup only speaks for
/// one window size (~128×41 cells); how it degrades is this file's decision.
pub const INSET_MIN_ROWS: u16 = 30;

/// Columns of ground between the sidebar's box and the main pane's. Measured
/// from the image: sidebar right edge ≈338px, main pane left edge ≈362px, at
/// ≈11.3px per cell — two columns, not shared borders. Zero when the sidebar
/// is collapsed: the gap is the sidebar's, and goes with it.
const PANE_GAP_COLS: u16 = 2;

/// Where everything goes, for one frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Regions {
    pub sidebar: Rect,
    /// The main pane's full rectangle — the border is drawn on it when
    /// `main_bordered`.
    pub main: Rect,
    pub main_bordered: bool,
    pub header: Rect,
    pub rule: Rect,
    pub chat: Rect,
    /// The input box, the menu above it, or the approval panel.
    pub dock: Rect,
    pub hint: Rect,
    pub status: Rect,
}

/// Carve the window. `sidebar_w` comes from [`sidebar::width`] and `dock_h`
/// from [`dock_height`], so the two decisions that depend on *state* are made
/// before the arithmetic that does not.
pub fn regions(area: Rect, sidebar_w: u16, dock_h: u16) -> Regions {
    // The mockup floats every box inside the window rather than flush against
    // it: ~14px of ground on an ~11.3px cell — one column each side, one row
    // above the panes and one below the bar. Shed first when the window is
    // small, because a float is the cheapest thing on screen; the horizontal
    // inset rides with the border threshold so the two shed as one look.
    let inset_x = u16::from(area.width >= UNBORDERED_COLS);
    let inset_y = u16::from(area.height >= INSET_MIN_ROWS);
    let outer = area.inner(Margin::new(inset_x, inset_y));
    let status_h = if area.height < SLIM_STATUS_ROWS { 1 } else { 3 };
    // One blank row between the main pane's bottom border and the bar's top —
    // measured, not styled: pane bottom ≈982px, bar top ≈1006px, one cell row.
    // It rides with the bar's border: a window too short for the border has no
    // row to spend on a gap either.
    let gap_h = u16::from(area.height >= SLIM_STATUS_ROWS);
    let [content, _, status] = Layout::vertical([
        Constraint::Min(0),
        Constraint::Length(gap_h),
        Constraint::Length(status_h),
    ])
    .areas(outer);
    let gap_w = if sidebar_w > 0 { PANE_GAP_COLS } else { 0 };
    let [sidebar, _, main] = Layout::horizontal([
        Constraint::Length(sidebar_w),
        Constraint::Length(gap_w),
        Constraint::Min(0),
    ])
    .areas(content);
    let main_bordered = area.width >= UNBORDERED_COLS && main.width >= 5 && main.height >= 3;
    // Inside the border the image pads the content: border at ≈362px, text at
    // ≈393px — the border cell plus one more column ((2,1) counts the border).
    let inner = if main_bordered {
        main.inner(Margin::new(2, 1))
    } else {
        main
    };
    let (header_h, rule_h) = if area.height < SLIM_HEADER_ROWS {
        (1, 0)
    } else {
        (2, 1)
    };
    let [header, rule, chat, dock, hint] = Layout::vertical([
        Constraint::Length(header_h),
        Constraint::Length(rule_h),
        Constraint::Min(1),
        Constraint::Length(dock_h),
        Constraint::Length(1),
    ])
    .areas(inner);
    Regions {
        sidebar,
        main,
        main_bordered,
        header,
        rule,
        chat,
        dock,
        hint,
        status,
    }
}

/// How many rows the dock needs: three for the input box, more while the menu
/// is up, and the approval panel's evidence when a question is pending. Capped
/// at half the pane — the prompt may grow upward into transcript rows (§4.6),
/// not through them.
pub fn dock_height(view: &View, room: u16, width: Option<u16>) -> u16 {
    let room = room.max(1);
    // The ceiling would like to be half the window and must never exceed the
    // window: on a degenerate frame half-of-room is below the floors, and a
    // clamp whose min exceeds its max is a panic, not a layout.
    let ceiling = (room / 2).max(5).min(room);
    let bounded = |want: u16, floor: u16| want.clamp(floor.min(ceiling), ceiling);
    match &view.prompt {
        Some(p) => bounded((p.preview.len() as u16).saturating_add(3), 5),
        None => match &view.menu {
            Some(m) => {
                let extra = (m.rows.len() as u16 + u16::from(m.note.is_some())).min(8);
                bounded(3 + extra, 3)
            }
            // The typed message decides, through the same function the box
            // paints with. Absent a width nothing can be wrapped, so the old
            // three rows are the floor and the answer when the caller does not
            // know yet.
            None => match width {
                Some(w) => bounded(super::view::dock_rows(view, w), 3),
                None => 3.min(ceiling),
            },
        },
    }
}

// endregion: The layout

// region: The app
// ---------------------------------------------------------------------------
// The app
// ---------------------------------------------------------------------------

/// Everything the full-screen frame keeps between paints that the inline
/// viewport never had to: the retained transcript, the sidebar posture, and
/// the chat pane's last-known size (which is what scrolling pages by and what
/// new blocks are wrapped to).
/// What occupies the main region — the space between the sidebar and the
/// status bar.
///
/// **The frame does not change; only this does.** Every mockup for the tool
/// pages keeps `SESSIONS`, `TOOLS`, `QUICK HELP` and the status row exactly as
/// the chat view has them, and replaces the middle. So a page is not a window,
/// not a mode, and not a second `App` — it is one value on this struct, read at
/// paint time.
///
/// `Chat` is not "no page". It is the ordinary occupant, and naming it that way
/// is what keeps the match exhaustive when a page is added: a new variant is a
/// compile error at every site that decides what to draw, rather than a silent
/// fall-through to the transcript.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    Settings,
    DataExplorer,
    Memory,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Pane {
    #[default]
    Chat,
    Page(Page),
}

#[derive(Debug)]
pub struct App {
    pub transcript: Transcript,
    /// What the main region is showing. See [`Pane`].
    pane: Pane,
    /// The showing page's rendered text, captured when that page was opened.
    ///
    /// One field, not one per page, because only one page shows at a time and a
    /// second `Vec` would be a second thing to forget to clear.
    ///
    /// **Scanned once, on open, rather than on every paint.** The store is a
    /// directory of files and reading it is real I/O; a frame repaints on every
    /// keystroke, every stream chunk and every resize, and a page that rescanned
    /// each time would put a directory walk on the render path. The snapshot is
    /// also the honest thing to show: a table that silently changed under the
    /// reader between two repaints would be worse than one that is plainly as of
    /// when it was opened.
    page_text: Vec<String>,
    latch: Latch,
    side: sidebar::State,
    /// The column entries are wrapped to: [`chat::message_width`] of the chat
    /// pane, so the wrapper and the painter agree on where the column ends —
    /// the one number the chat module insists both sides derive from it.
    wrap_width: u16,
    /// The chat pane's height at the last layout; less one row, a page.
    chat_height: u16,
    /// The sidebar's rectangle at the last layout, for click hit-testing:
    /// a mouse event arrives in window cells, and only the layout knows which
    /// cells were the sidebar's when the user aimed at them.
    side_rect: Rect,
}

impl App {
    pub fn new(size: (u16, u16)) -> Self {
        let (cols, rows) = size;
        let latch = Latch::default();
        let r = regions(
            Rect::new(0, 0, cols, rows),
            sidebar::width(cols, hidden(cols, latch)),
            3,
        );
        Self {
            transcript: Transcript::new(Cap::default()),
            pane: Pane::default(),
            page_text: Vec::new(),
            latch,
            side: sidebar::State {
                sessions: Vec::new(),
                commands: builtin_rows(),
                help: keymap(),
                collapsed: false,
            },
            wrap_width: chat::message_width(r.chat.width.max(1)),
            chat_height: r.chat.height.max(1),
            side_rect: r.sidebar,
        }
    }

    /// One block of transcript output. Each call is one [`Transcript`] entry —
    /// see the module doc for what that costs and why.
    pub fn push_block(&mut self, lines: Vec<Line<'static>>, skin: &Skin) {
        if lines.is_empty() {
            return;
        }
        self.transcript
            .push(EntryKind::Activity(lines), skin, self.wrap_width);
    }

    /// The goal the user typed, kept as a `User` entry so the chat pane's
    /// gutter can say `You` beside it — the kind is a fact, and flattening it
    /// to activity lines would make the pane guess it back from styling.
    pub fn push_user(&mut self, text: &str, skin: &Skin) {
        self.transcript
            .push(EntryKind::User(text.to_string()), skin, self.wrap_width);
    }

    /// Assistant prose, streaming. The tail entry re-renders whole on every
    /// delta — the win the design scores against the inline viewport's
    /// held-back partial line, which this frame does not need.
    pub fn stream(&mut self, delta: &str, skin: &Skin) {
        self.transcript.stream(delta, skin, self.wrap_width);
    }

    /// A page, as the keys mean it: the chat pane's height less one row of
    /// continuity.
    pub fn page(&self) -> usize {
        usize::from(self.chat_height.saturating_sub(1).max(1))
    }

    /// The user toggled the sidebar. Latches in both directions — see [`Latch`].
    pub fn toggle_sidebar(&mut self, total_cols: u16) {
        self.latch = Latch {
            collapsed: !hidden(total_cols, self.latch),
            by_user: true,
        };
    }

    /// A left click, in window cells. Returns whether anything changed, so the
    /// caller repaints only when there is something new to paint.
    ///
    /// The one click target stage 2 has is the sidebar's SESSIONS header row —
    /// where the `[-]` affordance sits, and the route the owner actually tried
    /// (the collapse defect report was "+ and -", not "Ctrl-B"). The whole
    /// header row is the target rather than the affordance's three cells: the
    /// affordance's exact columns are the sidebar's internal layout, which this
    /// file does not own, and a three-cell target at a guessed offset is a miss
    /// magnet — the row holds exactly one control, so the row is the control.
    ///
    /// A collapsed sidebar has no cells and therefore no click target; the
    /// way back is `Ctrl-B`, which the hint row names. That asymmetry is the
    /// design's own (§3.2): collapsed is fully hidden, not a rail.
    pub fn click(&mut self, x: u16, y: u16, total_cols: u16) -> bool {
        let r = self.side_rect;
        // Under three cells either way the sidebar drew nothing (its own
        // rule), so there is no affordance on screen to have been aimed at.
        if r.width < 3 || r.height < 3 {
            return false;
        }
        let header_row = r.y + 1;
        if y == header_row && x > r.x && x < r.right().saturating_sub(1) {
            self.toggle_sidebar(total_cols);
            return true;
        }
        false
    }

    /// The TOOLS section, from the user-tool catalogue — mapped by
    /// [`tool_rows`] and handed in as rows so this stays testable without the
    /// catalogue's filesystem probing.
    pub fn set_tools(&mut self, rows: Vec<sidebar::Row>) {
        self.side.commands = rows;
    }

    /// The run's identity arrived: the current session becomes the one
    /// (honestly known) row in SESSIONS, and the transcript learns where the
    /// complete record lives so the cap marker can point at it.
    pub fn set_identity(&mut self, session: &str, transcript_path: &str, skin: &Skin) {
        if !session.is_empty() {
            self.side.sessions = vec![sidebar::Row {
                name: session.to_string(),
                trailing: String::new(),
                selected: true,
            }];
        }
        if !transcript_path.is_empty() {
            self.transcript.record_at(transcript_path, skin);
        }
    }

    /// Draw the whole window. Returns where the cursor belongs, exactly as
    /// [`View::render`] does for the inline viewport.
    pub fn render(
        &mut self,
        area: Rect,
        buf: &mut Buffer,
        view: &View,
        bar: &statusbar::Bar,
    ) -> Option<Position> {
        if area.width == 0 || area.height == 0 {
            return None;
        }
        let skin = &view.skin;
        let collapsed = hidden(area.width, self.latch);
        self.side.collapsed = collapsed;
        let sb_w = sidebar::width(area.width, collapsed);
        let r = regions(
            area,
            sb_w,
            dock_height(view, area.height, Some(area.width.saturating_sub(sb_w))),
        );
        // Written down for the click hit-test: a mouse aims at what was on
        // screen at the last paint, which is exactly this rectangle.
        self.side_rect = r.sidebar;

        sidebar::render(r.sidebar, buf, &self.side, skin);

        if r.main_bordered {
            Block::bordered()
                .border_set(skin.glyphs.border)
                .border_style(skin.palette.dim())
                .render(r.main, buf);
        }
        match self.pane {
            Pane::Chat => self.chat_view(&r, view, buf),
            Pane::Page(Page::Settings) => self.settings_page(&r, view, buf),
            Pane::Page(Page::DataExplorer) => self.explorer_page(&r, view, buf),
            Pane::Page(Page::Memory) => self.text_page(
                &r,
                view,
                buf,
                "Memory",
                "what Emma remembers about this project",
                &[
                    "Emma has no embedding index and no retrieval, so the mockup's",
                    "  similarity scores and latency are not drawn. What she keeps is a",
                    "  record of what was asked and what happened, which is a real thing",
                    "  to show and a different one.",
                    "",
                    "Esc returns to the conversation.",
                ],
            ),
        }

        let cursor = match &view.prompt {
            Some(prompt) => view.render_prompt(prompt, r.dock, buf),
            None => view.render_input(r.dock, buf),
        };

        Line::from(Span::styled(
            fit(
                &self.hint(view),
                usize::from(r.hint.width),
                skin.glyphs.ellipsis,
            ),
            skin.palette.dim(),
        ))
        .render(r.hint, buf);
        self.status_row(&r, view, buf, bar, skin);
        cursor
    }

    /// Show a page, or go back to the transcript.
    ///
    /// The transcript is not discarded while a page is up — it is simply not
    /// painted — so returning to it costs nothing and loses no scroll position.
    pub fn show(&mut self, pane: Pane) {
        self.pane = pane;
    }

    pub fn pane(&self) -> Pane {
        self.pane
    }

    /// Hand the Data Explorer its snapshot and show it.
    ///
    /// The text is produced by `commands::sessions`, the same writer a command
    /// would print through — one implementation behind two surfaces, so the page
    /// and the command cannot come to disagree about what the store holds.
    pub fn show_explorer(&mut self, text: &str) {
        self.page_text = text.lines().map(str::to_string).collect();
        self.pane = Pane::Page(Page::DataExplorer);
    }

    /// The Memory page's snapshot, taken on open for the same reason.
    pub fn show_memory(&mut self, text: &str) {
        self.page_text = text.lines().map(str::to_string).collect();
        self.pane = Pane::Page(Page::Memory);
    }

    /// The Data Explorer.
    ///
    /// **The mockup wanted SQL over `notes.db`, and Emma has no database.**
    /// UI-002 was ruled to point this at Emma's own session logs, which is the
    /// only source where every figure on the page can be true on the day it
    /// ships. What is drawn here is a real count of real files.
    ///
    /// The mockup's query box, typed columns, `Elapsed: 12ms` and chart are not
    /// drawn, because none of them has anything behind it yet. A query surface
    /// over a store with no query engine would be the most convincing thing on
    /// the page and the least real.
    fn explorer_page(&mut self, r: &Regions, view: &View, buf: &mut Buffer) {
        self.text_page(
            r,
            view,
            buf,
            "Data Explorer",
            "what the session store holds, as of opening this page",
            &[
                "Not drawn, because nothing is behind it yet: the query box, typed",
                "  columns, elapsed time, and the chart. A query surface over a store",
                "  with no query engine would be the most convincing thing here and",
                "  the least real.",
                "",
                "Esc returns to the conversation.",
            ],
        );
    }

    /// A page that is a captured report plus a footer saying what it omits.
    ///
    /// **Both pages that read the session store have the same shape**, and the
    /// second one arriving is when that stops being a coincidence. The text is
    /// produced by a `commands::*` writer, so the page and the equivalent
    /// command cannot disagree; the footer is per page, because what each one
    /// deliberately does not draw is the part a reader most needs told.
    fn text_page(
        &mut self,
        r: &Regions,
        view: &View,
        buf: &mut Buffer,
        title: &str,
        subtitle: &str,
        footer: &[&str],
    ) {
        let skin = &view.skin;
        Line::from(vec![
            Span::styled(title.to_string(), skin.palette.bold(Role::Accent)),
            Span::styled(format!("   {subtitle}"), skin.palette.dim()),
        ])
        .render(r.header, buf);

        if r.rule.height > 0 {
            Line::from(Span::styled(
                skin.glyphs.rule.repeat(usize::from(r.rule.width)),
                skin.palette.dim(),
            ))
            .render(r.rule, buf);
        }

        let mut y = r.chat.y;
        let end = r.chat.y.saturating_add(r.chat.height);
        // What the page would draw with room enough, so a shortfall can be
        // stated rather than left as an absence. The `+ 1` is the blank row
        // between the report and the footer.
        let wanted = self.page_text.len() + 1 + footer.len();
        let mut drawn = 0usize;
        for line in &self.page_text {
            if y >= end {
                break;
            }
            drawn += 1;
            // Headings are the unindented, non-empty lines the writer emits;
            // everything else is data. Styling from shape rather than from a
            // parallel list, so the two cannot drift.
            let style = if line.starts_with(' ') || line.is_empty() {
                skin.palette.style(Role::Text)
            } else {
                skin.palette.bold(Role::Accent)
            };
            fit_spans(
                vec![Span::styled(line.clone(), style)],
                r.chat.width,
                skin.glyphs.ellipsis,
            )
            .render(Rect::new(r.chat.x, y, r.chat.width, 1), buf);
            y = y.saturating_add(1);
        }

        y = y.saturating_add(1);
        drawn += 1;
        for line in footer {
            if y >= end {
                break;
            }
            drawn += 1;
            fit_spans(
                vec![Span::styled(line.to_string(), skin.palette.dim())],
                r.chat.width,
                skin.glyphs.ellipsis,
            )
            .render(Rect::new(r.chat.x, y, r.chat.width, 1), buf);
            y = y.saturating_add(1);
        }
        note_cut(r, skin, buf, drawn as u16, wanted);
    }

    /// The Settings page.
    ///
    /// **Only what is real, which is the owner's ruling.** UI-001 was answered
    /// "(a), but only for what exists, we will add as we need to". So this draws
    /// the values Emma actually resolved at boot, each with where it came from,
    /// and says plainly which of the mockup's eight panels have nothing behind
    /// them yet rather than drawing a control that would not act.
    ///
    /// A settings screen showing a switch that changes nothing is the
    /// "declaration wearing the costume of a mechanism" this codebase refuses
    /// everywhere else — and it is worse here than most places, because a
    /// settings page is where a user goes *expecting* their change to take.
    fn settings_page(&mut self, r: &Regions, view: &View, buf: &mut Buffer) {
        let skin = &view.skin;
        let st = &view.status;
        Line::from(vec![
            Span::styled("Settings", skin.palette.bold(Role::Accent)),
            Span::styled(
                "   what this run resolved, and where each value came from",
                skin.palette.dim(),
            ),
        ])
        .render(r.header, buf);

        if r.rule.height > 0 {
            Line::from(Span::styled(
                skin.glyphs.rule.repeat(usize::from(r.rule.width)),
                skin.palette.dim(),
            ))
            .render(r.rule, buf);
        }

        let cap = |v: Option<(i64, i64)>| match v {
            Some((_, max)) if max > 0 => max.to_string(),
            _ => "not set".to_string(),
        };
        let rows: Vec<(&str, String, &str)> = vec![
            ("Model", st.model.clone(), "the run's provider settings"),
            (
                "Working directory",
                st.cwd.clone(),
                "where this session started",
            ),
            ("Session log", st.session.clone(), "the record of this run"),
            (
                "Context limit",
                cap(st.context),
                "compaction happens at this size",
            ),
            (
                "Per-goal budget",
                cap(st.spend),
                "a goal ends when it is spent",
            ),
        ];

        let mut y = r.chat.y;
        let end = r.chat.y.saturating_add(r.chat.height);
        // Counted the same way `text_page` counts, and for the same reason:
        // this page's whole argument is that an absent panel must read as
        // absent, and at 80x24 the panel that says so was itself off-screen.
        const SETTINGS_FOOTER: usize = 6;
        let wanted = rows.len() + 2 + keymap().len() + 1 + SETTINGS_FOOTER;
        let mut drawn = 0usize;
        let label_w = rows.iter().map(|(l, _, _)| l.len()).max().unwrap_or(0) + 2;
        for (label, value, why) in &rows {
            if y >= end {
                break;
            }
            drawn += 1;
            let line = fit_spans(
                vec![
                    Span::styled(format!("{label:<label_w$}"), skin.palette.style(Role::Text)),
                    Span::styled(value.clone(), skin.palette.bold(Role::Accent)),
                    Span::styled(format!("   {why}"), skin.palette.dim()),
                ],
                r.chat.width,
                skin.glyphs.ellipsis,
            );
            line.render(Rect::new(r.chat.x, y, r.chat.width, 1), buf);
            y = y.saturating_add(1);
        }

        y = y.saturating_add(1);
        drawn += 1;
        if y < end {
            drawn += 1;
            Line::from(Span::styled("Keys", skin.palette.bold(Role::Accent)))
                .render(Rect::new(r.chat.x, y, r.chat.width, 1), buf);
            y = y.saturating_add(1);
        }
        for (k, what) in keymap() {
            if y >= end {
                break;
            }
            drawn += 1;
            fit_spans(
                vec![
                    Span::styled(format!("{k:<label_w$}"), skin.palette.dim()),
                    Span::styled(what, skin.palette.style(Role::Text)),
                ],
                r.chat.width,
                skin.glyphs.ellipsis,
            )
            .render(Rect::new(r.chat.x, y, r.chat.width, 1), buf);
            y = y.saturating_add(1);
        }

        y = y.saturating_add(1);
        drawn += 1;
        for line in [
            "Not wired yet, and drawn as nothing rather than as a control:",
            "  temperature, streaming toggle, auto-summarise, memory retention,",
            "  keybinding editing, per-tool permission switches, telemetry.",
            "  Each needs a setting to exist before a control can mean anything.",
            "",
            "Esc returns to the conversation.",
        ] {
            if y >= end {
                break;
            }
            drawn += 1;
            fit_spans(
                vec![Span::styled(line.to_string(), skin.palette.dim())],
                r.chat.width,
                skin.glyphs.ellipsis,
            )
            .render(Rect::new(r.chat.x, y, r.chat.width, 1), buf);
            y = y.saturating_add(1);
        }
        note_cut(r, skin, buf, drawn as u16, wanted);
    }

    /// The ordinary occupant of the main region: header, rule, transcript.
    ///
    /// Extracted from `render` unchanged. It is a method rather than free
    /// function because it writes `wrap_width` and `chat_height` back onto the
    /// app — the two numbers a resize turns into a rewrap.
    fn chat_view(&mut self, r: &Regions, view: &View, buf: &mut Buffer) {
        let skin = &view.skin;
        self.header(r.header, view, buf);
        if r.rule.height > 0 {
            Line::from(Span::styled(
                skin.glyphs.rule.repeat(usize::from(r.rule.width)),
                skin.palette.dim(),
            ))
            .render(r.rule, buf);
        }

        // The width every retained entry is wrapped to is the message column
        // the chat pane will paint them into — [`chat::message_width`], which
        // subtracts the gutter, so the wrapper and the painter cannot
        // disagree about where the column ends. Set here, at the one place
        // both numbers are in hand: a resize is a rewrap, never a mismatch.
        self.wrap_width = chat::message_width(r.chat.width.max(1));
        self.chat_height = r.chat.height.max(1);
        self.transcript.set_width(skin, self.wrap_width);
        chat::render(r.chat, buf, &self.transcript, skin);
    }

    /// The status row and its border. Unchanged, extracted so `render` reads as
    /// the four things it decides rather than as one long paint.
    fn status_row(
        &self,
        r: &Regions,
        view: &View,
        buf: &mut Buffer,
        bar: &statusbar::Bar,
        skin: &Skin,
    ) {
        // The bar's border belongs to the shell — `statusbar::render` paints
        // one row of cells into whatever row it is handed, which is what lets
        // the border be shed below `SLIM_STATUS_ROWS` without the widget
        // knowing the window's height.
        let status_row = if r.status.height >= 3 {
            let block = Block::bordered()
                .border_set(skin.glyphs.border)
                .border_style(skin.palette.dim());
            let inner = block.inner(r.status);
            block.render(r.status, buf);
            inner
        } else {
            r.status
        };
        match &view.custom_status {
            // A configured status program replaces the built-in cells' row —
            // the same trade it makes inline, inside the same border the
            // built-in bar would have had.
            Some(text) => {
                super::statusline::to_line(
                    text,
                    view.status_padding,
                    skin.palette.style(Role::Text),
                )
                .render(status_row, buf);
            }
            None => statusbar::render(status_row, buf, bar, skin),
        }
    }

    /// The row under the input box: the keys nobody can guess. The
    /// scrolled-behind indicator is *not* here — the chat pane overlays it on
    /// its own last row, where the reader's eye already is.
    fn hint(&self, view: &View) -> String {
        let sep = view.skin.glyphs.sep;
        match view.mode {
            super::view::Mode::Working => format!(
                "Ctrl-C interrupts {sep} what you type now runs next {sep} PgUp/PgDn scrolls"
            ),
            super::view::Mode::Idle => {
                format!("type a goal {sep} / commands {sep} Ctrl-B sidebar {sep} PgUp/PgDn scrolls")
            }
        }
    }

    /// The header block: wordmark and version, the working directory as the
    /// subtitle. Every word of it is true of this run — the mockup's serif
    /// wordmark and its slogan are decoration a terminal cell cannot carry
    /// (design §3, Q8: one bold row, not figlet).
    fn header(&self, area: Rect, view: &View, buf: &mut Buffer) {
        if area.height == 0 || area.width == 0 {
            return;
        }
        let skin = &view.skin;
        let version = concat!("v", env!("CARGO_PKG_VERSION"));
        if area.height == 1 {
            Line::from(vec![
                Span::styled("Emma".to_string(), skin.palette.bold(Role::Accent)),
                Span::styled(
                    format!(" {} {version}", skin.glyphs.sep),
                    skin.palette.dim(),
                ),
            ])
            .render(area, buf);
            return;
        }
        let [word, subtitle] =
            Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).areas(area);
        let name = "Emma";
        let gap = usize::from(word.width)
            .saturating_sub(name.len() + version.len())
            .max(1);
        Line::from(vec![
            Span::styled(name.to_string(), skin.palette.bold(Role::Accent)),
            Span::raw(" ".repeat(gap)),
            Span::styled(version.to_string(), skin.palette.dim()),
        ])
        .render(word, buf);
        Line::from(Span::styled(
            fit(
                &view.status.cwd,
                usize::from(subtitle.width),
                skin.glyphs.ellipsis,
            ),
            skin.palette.dim(),
        ))
        .render(subtitle, buf);
    }
}

/// The command slot, filled with what is real: Emma's built-ins. Project
/// commands live in the `/` menu's vocabulary, which is composed in `main.rs`
/// and handed to the reader — the sidebar will get them when the shell is
/// handed the same list (stage 3's focus model needs it anyway).
fn builtin_rows() -> Vec<sidebar::Row> {
    crate::session_command::BUILTINS
        .iter()
        .map(|(name, _)| sidebar::Row {
            name: format!("/{name}"),
            trailing: String::new(),
            selected: false,
        })
        .collect()
}

/// The user-tool catalogue, mapped into the sidebar's [`sidebar::Row`]
/// contract for the TOOLS section.
///
/// The trailing column carries the *real* binding, not the mockup's bare
/// letter: the tools launch on `Alt+<key>` (see [`super::input::tool_key`] for
/// why a bare letter was refused), and Search's `/` is the command menu the
/// character already opens. `Row` has no availability field — the sidebar is
/// another agent's fixed contract — so the key column carries that truth
/// instead: a tool that cannot launch shows `n/a` where its chord would be,
/// because a rendered key that does nothing is the exact defect the design's
/// §6 forbids ("do not show keys that do nothing"). `Entry::detail` has no
/// cell to land in and is dropped, named here rather than silently.
/// Cut a composed line to the width it has, with the ellipsis the skin uses.
///
/// **The pages hard-truncated mid-token while the hint row one line below them
/// ellipsised correctly**, so two widgets on one screen disagreed about whether
/// a cut is announced. At 80 columns a Settings row read
/// `Session log  C:/…/sess.jsonl   the recor`, and in the Data Explorer a
/// record count could be cut *inside the number* — a truncated figure that
/// reads as a smaller figure, which is the "no silent wrong answers" rule
/// broken in the place it is cheapest to break it.
///
/// `render::fit` already exists and is the house convention; it takes a string,
/// and these lines are several styled spans. So the budget is spent span by
/// span: each gets what is left, and the one that runs out carries the
/// ellipsis. Spans past the edge are dropped rather than rendered empty.
fn fit_spans(spans: Vec<Span<'static>>, width: u16, ellipsis: &str) -> Line<'static> {
    let budget = usize::from(width);
    let mut out: Vec<Span<'static>> = Vec::with_capacity(spans.len());
    let mut used = 0usize;
    for span in spans {
        if used >= budget {
            break;
        }
        let left = budget - used;
        let text = span.content.as_ref();
        if crate::term::render::cols(text) <= left {
            used += crate::term::render::cols(text);
            out.push(span);
        } else {
            let cut = crate::term::render::fit(text, left, ellipsis);
            used = budget;
            out.push(Span::styled(cut, span.style));
        }
    }
    Line::from(out)
}

/// Say that a page ran out of room, on the last row it has.
///
/// **A page that silently stops is the defect these pages exist to avoid,
/// pointed inward.** Every one of them argues that an absent panel must read as
/// absent rather than as available — and at 80x24, the ordinary default, the
/// Settings page stopped after `PgUp/PgDn scroll` and the whole
/// "Not wired yet, and drawn as nothing rather than as a control" block was
/// off-screen, unreachable by any key, with nothing saying it existed. The Data
/// Explorer lost its entire "Not drawn" footer the same way. The three tests
/// that assert those blocks all render at 120x40, which is the smallest size at
/// which they fit; an adversarial reviewer rendered one at 80x24 and the
/// assertions were simply false there.
///
/// There is no scroll offset on these pages, so the remedy named is the only
/// one that exists today: a taller window. Saying that is worth more than
/// saying nothing, and much more than a page that looks complete.
///
/// Returns whether it drew, so a caller can tell "cut" from "fitted".
fn note_cut(r: &Regions, skin: &Skin, buf: &mut Buffer, drawn: u16, wanted: usize) -> bool {
    let end = r.chat.y.saturating_add(r.chat.height);
    if usize::from(drawn) >= wanted || r.chat.height == 0 {
        return false;
    }
    let hidden = wanted.saturating_sub(usize::from(drawn));
    let row = end.saturating_sub(1);
    fit_spans(
        vec![Span::styled(
            format!("[{hidden} more line(s) below the window; this page does not scroll — make the window taller]"),
            skin.palette.bold(Role::Warn),
        )],
        r.chat.width,
        skin.glyphs.ellipsis,
    )
    .render(Rect::new(r.chat.x, row, r.chat.width, 1), buf);
    true
}

pub fn tool_rows(entries: &[crate::usertools::Entry]) -> Vec<sidebar::Row> {
    entries
        .iter()
        .map(|e| sidebar::Row {
            name: e.label.clone(),
            trailing: if !e.available {
                "n/a".to_string()
            } else if e.key == '/' {
                "/".to_string()
            } else {
                format!("Alt+{}", e.key)
            },
            selected: false,
        })
        .collect()
}

/// The QUICK HELP table: the real keymap, nothing aspirational. `Shift+drag`
/// is here because mouse capture is on for the wheel, which takes plain
/// drag-selection away — the key that gives it back is the one fact a user
/// cannot guess.
///
/// **The rows used to be literals here, and that is the arrangement that
/// produced the defect this now cannot have.** A `(key, description)` pair
/// typed by hand carries no reference to the `match` arm that answers it, so
/// the two drift and nothing says so —
/// `notes/design/tui-fork-inventory.md` §11 counted a branch's copy of this
/// panel advertising six keys of which four do nothing, one of them `Ctrl+k`
/// for a binding that is `Ctrl-U`. The rows now come from
/// [`super::bindings::CHAT`], where each carries the chord it means; the
/// printed spelling is derived from that chord rather than typed beside it,
/// and the tests there drive every one through the real decoders. Both paint
/// sites — the sidebar's panel and the Settings page's `Keys` block — read
/// this one value, so there is still exactly one table on screen.
pub(crate) fn keymap() -> Vec<(String, String)> {
    super::bindings::CHAT.hints()
}

/// The last path component, for the status bar's ENV cell. Local rather than
/// borrowed from `render.rs`, whose copy is private and whose signature may
/// move with the cell renderers.
pub fn stem(path: &str) -> String {
    path.rsplit(['/', '\\'])
        .find(|p| !p.is_empty())
        .unwrap_or(path)
        .to_string()
}

// endregion: The app

#[cfg(test)]
mod tests {
    use super::super::palette::{Level, Palette};
    use super::super::render::UNICODE;
    use super::super::view::{Mode, Prompt};
    use super::*;

    fn skin() -> Skin {
        Skin::new(Palette::new(Level::Truecolor), UNICODE)
    }

    fn view() -> View {
        let mut v = View::new(skin());
        v.status.model = "claude-opus-4".into();
        v.status.cwd = "C:\\src\\emma".into();
        // **The same numbers the status-bar fixture carries.** They were unset
        // here and set there, so `show_the_pages` printed a Settings panel
        // saying "Per-goal budget  not set" three rows above a status bar
        // saying "TOKENS 0/500,000". A person asked to judge whether a page
        // reads well was shown a screen contradicting itself, from fixture
        // drift rather than from anything the product does.
        v.status.session = "C:/Users/you/.emma/sessions/2026-08-23T11-02-55.jsonl".into();
        v.status.context = Some((0, 120_000));
        v.status.spend = Some((0, 500_000));
        v
    }

    fn bar() -> statusbar::Bar {
        statusbar::Bar {
            mode: "IDLE".into(),
            model: "claude-opus-4".into(),
            env: "emma".into(),
            ctx_used: 0,
            ctx_max: 120_000,
            total_used: 0,
            total_max: 500_000,
            up: None,
            down: None,
            elapsed: None,
        }
    }

    fn draw(app: &mut App, v: &View, w: u16, h: u16) -> (Vec<String>, Option<Position>) {
        let area = Rect::new(0, 0, w, h);
        let mut buf = Buffer::empty(area);
        let cursor = app.render(area, &mut buf, v, &bar());
        let rows = (0..h)
            .map(|y| {
                (0..w)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect();
        (rows, cursor)
    }

    // -----------------------------------------------------------------------
    // The pane seam
    // -----------------------------------------------------------------------

    /// The Settings page replaces the transcript and nothing else.
    ///
    /// **The frame is the invariant.** Every mockup keeps the sidebar and the
    /// status bar and swaps only the middle, so a page that took the whole
    /// screen would be a different product. This checks the frame survived and
    /// that the transcript did not paint underneath.
    ///
    /// It also checks the page says what it cannot do. UI-001 was ruled "only
    /// for what exists", and a settings screen is the one place a user arrives
    /// *expecting* a control to take — so a panel with nothing behind it has to
    /// read as absent rather than as available.
    /// Print the three pages, for a human who wants to look at them.
    ///
    /// **A cell buffer is not a console, and this is a picture of the buffer.**
    /// It shows the layout, the wording and what each page refuses to draw; it
    /// cannot show colour, cannot show that Windows Terminal delivered `Alt+,`,
    /// and cannot show tearing. Everything it proves is already asserted by the
    /// three tests below it. What it is for is the question those tests cannot
    /// answer — *does this read well* — which only a person can settle.
    ///
    ///     cargo test -p emma --lib term::app::tests::show_the_pages -- --ignored --nocapture
    #[test]
    #[ignore = "prints for a human to read; asserts nothing"]
    fn show_the_pages() {
        let v = view();
        let sample = concat!(
            "source         C:\\Users\\you\\.emma\\sessions\n",
            "sessions       3 file(s), 412 record(s)\n",
            "\n",
            "kinds\n",
            "  assistant        118\n",
            "  goal               3\n",
            "  tool_result       97\n",
            "\n",
            "sessions, newest last\n",
            "  2026-08-21T09-14-02              88 records       31 KiB\n",
            "  2026-08-23T11-02-55             206 records       74 KiB\n",
        );
        let memory_sample = concat!(
            "project        C:\\src\\emma\n",
            "sessions here  3\n",
            "goals asked    3\n",
            "compactions    1 - a summary replaced older turns this many times\n",
            "clears         0\n",
            "\n",
            "what was asked here, newest last\n",
            "  2026-08-21T09-14-02            harden the permission matcher\n",
            "  2026-08-23T11-02-55            make Alt+, open a page, not my editor\n",
        );
        for (title, page) in [
            ("SETTINGS", Pane::Page(Page::Settings)),
            ("DATA EXPLORER", Pane::Page(Page::DataExplorer)),
            ("MEMORY", Pane::Page(Page::Memory)),
        ] {
            let mut a = App::new((120, 40));
            a.set_tools(tool_rows(&crate::usertools::catalogue(
                std::path::Path::new("."),
            )));
            match page {
                Pane::Page(Page::DataExplorer) => a.show_explorer(sample),
                Pane::Page(Page::Memory) => a.show_memory(memory_sample),
                other => a.show(other),
            }
            let (rows, _) = draw(&mut a, &v, 120, 40);
            println!("\n===== {title} =====");
            for r in rows {
                println!("{}", r.trim_end());
            }
        }
    }

    /// A page too small to hold its own honesty says so.
    ///
    /// **The three page tests all render at 120x40, which is the smallest size
    /// at which the disclosure fits.** At 80x24 — an ordinary default — the
    /// Settings page stopped after `PgUp/PgDn scroll`, and the whole
    /// "Not wired yet, and drawn as nothing rather than as a control" block was
    /// off-screen, unreachable by any key, with nothing on screen saying it
    /// existed. The Data Explorer lost its "Not drawn" footer the same way. An
    /// adversarial reviewer rendered them and the assertions the rows cite were
    /// simply false there.
    ///
    /// That is this project's own rule turned inward: a panel that is absent
    /// must read as absent. A page whose honesty is the part that got cut is
    /// the worst possible thing to cut silently.
    #[test]
    fn a_page_that_runs_out_of_room_says_how_much_is_missing() {
        let v = view();
        for (name, pane) in [
            ("Settings", Pane::Page(Page::Settings)),
            ("Data Explorer", Pane::Page(Page::DataExplorer)),
        ] {
            let mut a = App::new((80, 24));
            match pane {
                Pane::Page(Page::DataExplorer) => a.show_explorer(
                    "source         C:/x\nsessions       3 file(s)\n\nkinds\n  goal   3\n",
                ),
                other => a.show(other),
            }
            let (rows, _) = draw(&mut a, &v, 80, 24);
            let screen = rows.join("\n");
            assert!(
                screen.contains("more line(s) below the window"),
                "{name} at 80x24 dropped content and said nothing:\n{screen}"
            );
            assert!(
                screen.contains("does not scroll"),
                "the notice must name the remedy, not just the loss:\n{screen}"
            );
        }

        // The positive control, and it is load-bearing: a notice that fired at
        // every size would be noise, and would also make the assertion above
        // pass for the wrong reason.
        let mut a = App::new((120, 40));
        a.show(Pane::Page(Page::Settings));
        let (rows, _) = draw(&mut a, &v, 120, 40);
        let screen = rows.join("\n");
        assert!(
            !screen.contains("more line(s) below the window"),
            "a page that fitted claimed it had been cut:\n{screen}"
        );
        assert!(
            screen.contains("Not wired yet"),
            "the disclosure that must survive did not:\n{screen}"
        );
    }

    #[test]
    fn the_settings_page_swaps_the_middle_and_keeps_the_frame() {
        let v = view();
        let mut a = App::new((120, 40));
        a.set_tools(vec![sidebar::Row {
            name: "Shell".into(),
            trailing: "Alt+s".into(),
            selected: false,
        }]);
        a.show(Pane::Page(Page::Settings));
        let (rows, _) = draw(&mut a, &v, 120, 40);
        let screen = rows.join(
            "
",
        );

        assert!(
            screen.contains("Settings"),
            "the page did not draw:
{screen}"
        );
        assert!(
            screen.contains("SESSIONS"),
            "the sidebar went with it:
{screen}"
        );
        assert!(
            screen.contains("MODE"),
            "the status bar went with it:
{screen}"
        );
        assert!(
            screen.contains("Not wired yet"),
            "the page does not say which controls have nothing behind them:
{screen}"
        );

        // And back, with the transcript intact — a page is a view, not a mode
        // that discards state.
        a.show(Pane::Chat);
        let (rows, _) = draw(&mut a, &v, 120, 40);
        assert!(
            !rows
                .join(
                    "
"
                )
                .contains("Not wired yet"),
            "the page kept painting after leaving it"
        );
    }

    /// The Data Explorer draws its snapshot, and says what it will not draw.
    ///
    /// **The mockup's query box and `Elapsed: 12ms` are absent on purpose.**
    /// UI-002 pointed this page at the session logs because that is the only
    /// source where its figures can be true; a query surface over a store with
    /// no query engine would be the most convincing thing on the page and the
    /// least real. This asserts the absence, because "we left it out honestly"
    /// and "we forgot" look identical in a screenshot.
    #[test]
    fn the_explorer_page_draws_its_snapshot_and_names_what_it_omits() {
        let v = view();
        let mut a = App::new((120, 40));
        a.set_tools(vec![sidebar::Row {
            name: "Shell".into(),
            trailing: "Alt+s".into(),
            selected: false,
        }]);
        a.show_explorer(
            "source         C:/store
sessions       2 file(s), 9 record(s)",
        );
        let (rows, _) = draw(&mut a, &v, 120, 40);
        let screen = rows.join(
            "
",
        );

        assert!(
            screen.contains("Data Explorer"),
            "the page did not draw:
{screen}"
        );
        assert!(
            screen.contains("C:/store"),
            "the snapshot is missing:
{screen}"
        );
        assert!(
            screen.contains("9 record(s)"),
            "the counts are missing:
{screen}"
        );
        assert!(
            screen.contains("SESSIONS"),
            "the sidebar went with it:
{screen}"
        );
        assert!(
            screen.contains("Not drawn"),
            "the page does not say what it is leaving out:
{screen}"
        );
    }

    /// Memory draws what Emma keeps, and refuses the mockup's retrieval block.
    ///
    /// `Total memories: 342`, `Embedding model: all-MiniLM-L6-v2`,
    /// `Retrieval latency: 42ms` — none of those has a source in this
    /// repository. The ruling put this page on Emma's own session memory, which
    /// is a real thing and a different one, and the page has to say which.
    #[test]
    fn the_memory_page_says_it_has_no_index_rather_than_inventing_one() {
        let v = view();
        let mut a = App::new((120, 40));
        a.set_tools(vec![sidebar::Row {
            name: "Shell".into(),
            trailing: "Alt+s".into(),
            selected: false,
        }]);
        a.show_memory(
            "project        C:/src/emma
goals asked    12",
        );
        let (rows, _) = draw(&mut a, &v, 120, 40);
        let screen = rows.join(
            "
",
        );

        assert!(
            screen.contains("Memory"),
            "the page did not draw:
{screen}"
        );
        assert!(
            screen.contains("goals asked"),
            "the snapshot is missing:
{screen}"
        );
        assert!(
            screen.contains("SESSIONS"),
            "the sidebar went with it:
{screen}"
        );
        assert!(
            screen.contains("no embedding index"),
            "the page does not say it has no retrieval, which invites the reader to              assume the mockup's numbers are somewhere:
{screen}"
        );
    }

    /// Every page names the one key that gets out of it.
    ///
    /// **There is no other way off a page.** `Esc` is it — no scroll, no
    /// close affordance, no click target — so the sentence saying so is not
    /// decoration, it is the exit. If it were deleted the user would see three
    /// pages that look like modes with no visible way back, and the recorded
    /// shape of that complaint in this repository is somebody pressing the
    /// chord again and reporting that it "does not close".
    ///
    /// The chat pane is the control: it must *not* carry the sentence, or the
    /// assertion above is satisfied by a string that lives somewhere in the
    /// frame rather than on the page. (`Esc` still appears in the sidebar's
    /// QUICK HELP as `close menu / leave page`, which is a different claim in
    /// different words, and is why the assertion is on the whole sentence.)
    ///
    /// **A cell buffer is not a console.** This proves the characters were
    /// written into the cells the layout chose. It does not prove a terminal
    /// painted them, that they were not overdrawn by a later widget on a real
    /// screen, or that `Esc` arrives at all through the reader.
    #[test]
    fn every_page_names_the_key_that_leaves_it() {
        const EXIT: &str = "Esc returns to the conversation.";
        let v = view();
        for (name, pane) in [
            ("Settings", Pane::Page(Page::Settings)),
            ("Data Explorer", Pane::Page(Page::DataExplorer)),
            ("Memory", Pane::Page(Page::Memory)),
        ] {
            let mut a = App::new((120, 40));
            match pane {
                Pane::Page(Page::DataExplorer) => a.show_explorer("source  C:/store"),
                Pane::Page(Page::Memory) => a.show_memory("project  C:/src/emma"),
                other => a.show(other),
            }
            let (rows, _) = draw(&mut a, &v, 120, 40);
            let screen = rows.join("\n");
            assert!(
                screen.contains(EXIT),
                "{name} does not say how to leave it, and nothing else does:\n{screen}"
            );
        }

        let mut a = App::new((120, 40));
        let (rows, _) = draw(&mut a, &v, 120, 40);
        let screen = rows.join("\n");
        assert!(
            !screen.contains(EXIT),
            "the conversation tells you how to return to itself, so the assertions \
             above pass without any page drawing anything:\n{screen}"
        );
    }

    /// Each page's disclosure is that page's, and no other page claims it.
    ///
    /// **Two of the three pages are the same function** — Memory and the Data
    /// Explorer are both `text_page`, differing only in the footer handed in —
    /// so the failure that is actually available here is a footer landing on
    /// the wrong page, or one page's footer being drawn for all of them. A
    /// reader who saw `no embedding index` under the Data Explorer would
    /// conclude the session store is an index that failed, which is a worse
    /// state than no sentence at all.
    ///
    /// Written as a matrix rather than three `contains` calls because the
    /// positive half alone is what the three existing page tests already do,
    /// and each of them stays green if its sentence is copied onto all three
    /// pages. The negatives are the part that can fail.
    ///
    /// **A cell buffer is not a console**: this establishes which strings the
    /// layout wrote for which pane, nothing about what a terminal renders.
    #[test]
    fn each_pages_disclosure_belongs_to_that_page_alone() {
        // The sentence that is the whole argument of each page, in the words
        // the page uses.
        const SETTINGS: &str = "Not wired yet";
        const EXPLORER: &str = "Not drawn, because nothing is behind it yet";
        const MEMORY: &str = "no embedding index";

        let v = view();
        let render = |pane: Pane| {
            let mut a = App::new((120, 40));
            match pane {
                Pane::Page(Page::DataExplorer) => a.show_explorer("source  C:/store"),
                Pane::Page(Page::Memory) => a.show_memory("project  C:/src/emma"),
                other => a.show(other),
            }
            let (rows, _) = draw(&mut a, &v, 120, 40);
            rows.join("\n")
        };

        let settings = render(Pane::Page(Page::Settings));
        let explorer = render(Pane::Page(Page::DataExplorer));
        let memory = render(Pane::Page(Page::Memory));

        for (name, screen, mine, theirs) in [
            ("Settings", &settings, SETTINGS, [EXPLORER, MEMORY]),
            ("Data Explorer", &explorer, EXPLORER, [SETTINGS, MEMORY]),
            ("Memory", &memory, MEMORY, [SETTINGS, EXPLORER]),
        ] {
            assert!(
                screen.contains(mine),
                "{name} lost its own disclosure:\n{screen}"
            );
            for other in theirs {
                assert!(
                    !screen.contains(other),
                    "{name} is carrying another page's disclosure ({other:?}), which \
                     tells the reader the wrong thing is missing:\n{screen}"
                );
            }
        }
    }

    /// The Data Explorer names the four things it will not draw, one by one.
    ///
    /// **`Not drawn` on its own is a shrug.** The existing test asserts that
    /// phrase, and it stays green if the list behind it is replaced with
    /// nothing, or with a different four items. The page's claim (UI-002) is
    /// specific: the query box, the typed columns, the elapsed time and the
    /// chart are absent because there is no query engine — and the query box is
    /// the one a user will otherwise spend a minute hunting for, because every
    /// data explorer they have ever used has one.
    ///
    /// This is also the standard the Memory page does not meet: the Explorer
    /// names its omitted input surface, Memory's footer speaks only about
    /// embeddings and never about the composer its own sidebar row promises.
    /// That gap is recorded in `memory_has_no_composer_and_the_page_does_not_
    /// mention_one` rather than asserted here, because it is a defect to fix,
    /// not a behaviour to pin.
    ///
    /// **A cell buffer is not a console**: cells written, not pixels shown.
    #[test]
    fn the_explorer_names_the_query_box_among_what_it_will_not_draw() {
        let v = view();
        let mut a = App::new((120, 40));
        a.show_explorer("source  C:/store\nsessions  2 file(s), 9 record(s)");
        let (rows, _) = draw(&mut a, &v, 120, 40);
        let screen = rows.join("\n");
        for absent in ["query box", "columns", "elapsed time", "chart"] {
            assert!(
                screen.contains(absent),
                "the page does not say the {absent} is missing, so its absence reads \
                 as an oversight rather than a refusal:\n{screen}"
            );
        }
    }

    /// Settings shows this run's own values, and says where each one came from.
    ///
    /// **The page's subtitle is a promise — "what this run resolved, and where
    /// each value came from"** — and nothing checked either half. A one-line
    /// change swapping `st.model` for a literal, or dropping the third column,
    /// left every existing assertion green, because they all key off the
    /// `Not wired yet` block at the bottom.
    ///
    /// The model string is deliberately not the one the status-bar fixture
    /// carries: `contains` over the whole screen would otherwise be satisfied
    /// by the status bar, three rows below, and the test would pass with the
    /// Settings row blank. Each assertion is on a single row holding both the
    /// value and its provenance, which is the pairing the page promises.
    ///
    /// The last one is the honesty branch: an unset budget must read `not set`,
    /// not `0`. `0` on a settings page reads as a configured limit of zero.
    ///
    /// **A cell buffer is not a console.** It proves the row was composed and
    /// written; it cannot prove the terminal showed all of it, and at narrower
    /// widths `fit_spans` will cut the provenance column off the right edge —
    /// which is why this renders wide, and why the cut is a separate test.
    #[test]
    fn the_settings_page_shows_this_runs_values_beside_where_each_came_from() {
        let mut v = view();
        v.status.model = "a-model-only-this-test-sets".into();
        v.status.session = "C:/Users/you/.emma/sessions/only-this-test.jsonl".into();
        v.status.context = Some((0, 120_000));
        // Unset on purpose: the branch that has to say so rather than say zero.
        v.status.spend = None;

        let mut a = App::new((140, 40));
        a.show(Pane::Page(Page::Settings));
        let (rows, _) = draw(&mut a, &v, 140, 40);
        let screen = rows.join("\n");

        for (what, value, why) in [
            (
                "the model",
                "a-model-only-this-test-sets",
                "provider settings",
            ),
            (
                "the session log",
                "only-this-test.jsonl",
                "record of this run",
            ),
            (
                "the context limit",
                "120000",
                "compaction happens at this size",
            ),
        ] {
            assert!(
                rows.iter().any(|r| r.contains(value) && r.contains(why)),
                "{what} is not shown beside where it came from — one row has to \
                 carry both, or the page's subtitle is not true:\n{screen}"
            );
        }

        assert!(
            rows.iter()
                .any(|r| r.contains("Per-goal budget") && r.contains("not set")),
            "an unset budget did not read as unset; a number here reads as a \
             configured limit:\n{screen}"
        );
    }

    /// Memory has no composer, and the page does not mention one.
    ///
    /// **This is a finding, not a feature.** The sidebar row and the mockup
    /// notes describe Memory as "the only page with its own composer"
    /// (`Ask Emma about your memory…`). There is no composer in this
    /// repository: `Pane::Page(Page::Memory)` goes to the shared `text_page`
    /// with the same call shape as the Data Explorer, and the only input on
    /// screen is the chat dock `render` draws for every pane after the match.
    ///
    /// What is asserted here is the true half — **there is exactly one input
    /// box on the Memory page, and it is the chat one, unchanged**. That is
    /// worth pinning in its own right: the thing this codebase would actually
    /// do wrong is draw a second box that looks like a composer and swallows
    /// nothing, which is the "declaration wearing the costume of a mechanism"
    /// the Settings page's doc refuses. Two boxes, or a dock that differs
    /// between Memory and the conversation, fails here.
    ///
    /// What is *not* asserted, and is a live defect: the Memory footer
    /// discloses only the missing embeddings, while the Data Explorer's names
    /// its own omitted query box. Same shortfall, one page owns up to it.
    /// Pinning the silence would make the fix turn this test red, so it is
    /// written down instead — see `the_explorer_names_the_query_box_among_what_
    /// it_will_not_draw`.
    ///
    /// **A cell buffer is not a console**, and it matters more here than
    /// elsewhere: identical cells in two buffers say nothing about whether a
    /// terminal repainted the dock when the pane changed under it.
    #[test]
    fn memory_has_no_composer_and_the_page_does_not_mention_one() {
        let v = view();
        let tools = vec![sidebar::Row {
            name: "Memory".into(),
            trailing: "Alt+m".into(),
            selected: false,
        }];

        let mut chat = App::new((120, 40));
        chat.set_tools(tools.clone());
        let (chat_rows, _) = draw(&mut chat, &v, 120, 40);

        let mut mem = App::new((120, 40));
        mem.set_tools(tools);
        mem.show_memory("project        C:/src/emma\ngoals asked    12");
        let (mem_rows, _) = draw(&mut mem, &v, 120, 40);

        // `[send: Enter]` is drawn once per input box, by the box itself.
        // Counting it counts boxes.
        let boxes = mem_rows
            .iter()
            .filter(|r| r.contains("[send: Enter]"))
            .count();
        assert_eq!(
            boxes,
            1,
            "the Memory page draws {boxes} input boxes; it has one, and it is the \
             conversation's:\n{}",
            mem_rows.join("\n")
        );

        // The dock is the shared one, cell for cell, not a page-local copy that
        // could drift into a composer.
        let area = Rect::new(0, 0, 120, 40);
        let r = regions(
            area,
            sidebar::width(120, hidden(120, Latch::default())),
            dock_height(&v, 40, None),
        );
        let slice = |rows: &[String]| {
            (r.dock.y..r.dock.y.saturating_add(r.dock.height))
                .map(|y| rows[usize::from(y)].clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            slice(&mem_rows),
            slice(&chat_rows),
            "the Memory page's input dock is not the conversation's — a page-local \
             input is the composer this repository does not have"
        );
    }

    /// The three page chords the sidebar advertises are chords the key decoder
    /// actually returns.
    ///
    /// **A rendered key that does nothing is the defect `tool_rows`' own doc
    /// names**, and it has already shipped once: the sidebar showed `Alt+M`
    /// while `launch_tool` sent the key to `launch`, which refused it with a
    /// warning about a key the frame had never claimed. The half that was
    /// missing was not the chord and not the page, it was the agreement
    /// between them.
    ///
    /// This walks the real catalogue rather than a fixture, because a fixture
    /// asserts what the fixture's author believed. For the three pages the
    /// catalogue is machine-independent — `catalogue_on` takes their
    /// availability from `Tool::routed()` and never probes the box — so this is
    /// deterministic on any developer's machine, unlike the launching tools
    /// beside them.
    ///
    /// It stops at the decoder. **Nothing here proves a terminal delivers
    /// `Alt+,`** — Windows Terminal's handling of that chord is exactly the
    /// thing a buffer cannot speak for, and the only evidence for it is
    /// somebody pressing the key.
    #[test]
    fn the_page_chords_the_sidebar_advertises_are_chords_the_decoder_returns() {
        use super::super::input::tool_key;
        use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let entries = crate::usertools::catalogue(std::path::Path::new("."));
        for (label, key) in [("Settings", ','), ("Memory", 'm'), ("Data Explorer", 'd')] {
            let entry = entries
                .iter()
                .find(|e| e.label == label)
                .unwrap_or_else(|| panic!("{label} is not in the catalogue at all"));
            let row = &tool_rows(std::slice::from_ref(entry))[0];
            assert_eq!(
                row.trailing,
                format!("Alt+{key}"),
                "the sidebar does not offer {label} a working chord; `n/a` here means \
                 the page is unreachable from the panel that names it"
            );
            assert_eq!(
                tool_key(KeyEvent::new(KeyCode::Char(key), KeyModifiers::ALT), false),
                Some(key),
                "the sidebar advertises Alt+{key} for {label} and the decoder drops it"
            );
        }
    }

    /// The chat view goes through the pane match, and nothing else moved.
    ///
    /// **Story 1 of the pane plan is a refactor, and a refactor's whole claim is
    /// that it changed nothing.** `render` used to paint the main region inline;
    /// it now matches on `Pane` and calls `chat_view`. This asserts the claim
    /// rather than trusting the diff: the same input must produce the same
    /// screen, and the cursor must still come back.
    ///
    /// It names each piece of the frame rather than counting rows. A test that
    /// checked one region only would pass while the header, the rule, the dock
    /// or the status row had quietly moved — and those are exactly what an
    /// extraction of this shape puts at risk, because each was a statement in
    /// the function being split.
    #[test]
    fn the_pane_seam_paints_the_chat_view_and_leaves_the_frame_alone() {
        let v = view();
        let mut a = App::new((100, 30));
        let (rows, cursor) = draw(&mut a, &v, 100, 30);

        assert_eq!(a.pane, Pane::Chat, "the default occupant is not the chat");
        assert!(cursor.is_some(), "the seam swallowed the cursor position");

        // The frame: sidebar on the left, status at the foot. Named rather than
        // counted, so a failure says which piece went missing.
        let screen = rows.join("\n");
        assert!(
            screen.contains("SESSIONS"),
            "the sidebar is gone:\n{screen}"
        );
        assert!(
            screen.contains("TOOLS"),
            "the tools panel is gone:\n{screen}"
        );
        assert!(screen.contains("MODE"), "the status bar is gone:\n{screen}");
    }

    // -----------------------------------------------------------------------
    // The latch
    // -----------------------------------------------------------------------

    /// The design's §3.2 stickiness table, as a state machine: automatic
    /// collapse yields to the window, a user's choice yields to nothing but
    /// the user.
    #[test]
    fn the_sidebar_latch_honours_the_user_over_the_width() {
        let auto = Latch::default();
        // Automatic: the width decides, both ways.
        assert!(hidden(80, auto));
        assert!(!hidden(120, auto));
        // A user collapse above the threshold latches...
        let user_closed = Latch {
            collapsed: true,
            by_user: true,
        };
        assert!(hidden(200, user_closed));
        // ...and a user expand below it is honoured: they insisted, the
        // layout obeys, the main pane gets narrow.
        let user_open = Latch {
            collapsed: false,
            by_user: false,
        };
        assert!(hidden(80, user_open), "precondition: auto would hide it");
        let mut app = App::new((80, 24));
        app.toggle_sidebar(80); // auto-hidden -> user expands
        assert!(!hidden(80, app.latch), "the user's expand was overruled");
        app.toggle_sidebar(80); // user collapses again
        assert!(hidden(80, app.latch));
    }

    // -----------------------------------------------------------------------
    // The layout
    // -----------------------------------------------------------------------

    #[test]
    fn a_roomy_window_gets_all_three_regions_at_the_mockups_proportions() {
        let r = regions(Rect::new(0, 0, 120, 30), sidebar::width(120, false), 3);
        assert_eq!(r.sidebar.width, 28, "{r:?}");
        assert_eq!(r.status.height, 3, "{r:?}");
        assert!(r.main_bordered);
        // The main pane is what the sidebar, the pane gap and the two-sided
        // float left; the chat area is inside its border plus one padding
        // column each side.
        assert_eq!(r.main.width, 120 - 28 - 2 - 2);
        assert_eq!(r.chat.width, r.main.width - 4);
        // Bottom-up inside the pane: chat, dock, hint.
        assert!(r.chat.y > r.header.y);
        assert_eq!(r.dock.y, r.chat.bottom());
        assert_eq!(r.hint.y, r.dock.bottom());
        assert_eq!(r.status.y, 26);
    }

    /// **The three gaps measured off the image itself (2026-08-13), pinned.**
    /// The design note's §1.1 sampled table, though pixel-derived, recorded
    /// none of them; the image outranks it (§1's own rule). At the mockup's
    /// approximate cell size (1448×1086px at ~11.3×26px per cell ≈ 128×41):
    /// panes float one cell inside the window, two columns of ground separate
    /// the sidebar's box from the main pane's, and one blank row separates the
    /// main pane's bottom border from the status bar's top.
    #[test]
    fn the_pane_gaps_measured_from_the_mockup_are_in_the_carve() {
        let r = regions(Rect::new(0, 0, 128, 41), sidebar::width(128, false), 3);
        // The float: nothing touches the window edge on a roomy window.
        assert_eq!(r.sidebar.x, 1, "{r:?}");
        assert_eq!(r.sidebar.y, 1, "{r:?}");
        assert_eq!(r.status.bottom(), 41 - 1, "{r:?}");
        // Two columns of ground between the boxes — they do not share an edge.
        assert_eq!(r.main.x - r.sidebar.right(), 2, "{r:?}");
        // One blank row between the pane's bottom border and the bar's top.
        assert_eq!(r.status.y - r.main.bottom(), 1, "{r:?}");
        // Collapsed, the pane gap goes with the sidebar rather than becoming
        // a stray two-column stripe of nothing.
        let r = regions(Rect::new(0, 0, 128, 41), 0, 3);
        assert_eq!(r.main.x, 1, "{r:?}");
    }

    /// **The seam that would fail silently.** The transcript is wrapped by
    /// this shell and painted by the chat pane at a gutter offset; if the two
    /// widths are derived separately, every wrapped row is clipped by the
    /// gutter's width and no test in either file notices. So the shell's
    /// number is pinned to the pane's own function of the pane's own rect.
    #[test]
    fn the_wrap_width_is_the_chat_panes_message_width_exactly() {
        let mut app = App::new((120, 30));
        let _ = draw(&mut app, &view(), 120, 30);
        let r = regions(
            Rect::new(0, 0, 120, 30),
            sidebar::width(120, false),
            dock_height(&view(), 30, None),
        );
        assert_eq!(app.wrap_width, chat::message_width(r.chat.width));
        assert!(
            app.wrap_width < r.chat.width,
            "at 120 columns the pane affords a gutter, so the two must differ"
        );
    }

    /// The evaluation's must-change #4: at 80×24 the plan itself says the
    /// sidebar must be collapsed, so stage 2 ships the collapse logic rather
    /// than a width floor.
    #[test]
    fn at_eighty_by_twenty_four_the_sidebar_is_auto_collapsed() {
        let mut app = App::new((80, 24));
        let (rows, cursor) = draw(&mut app, &view(), 80, 24);
        // No sidebar columns: the main pane's border sits at the float's one
        // inset column, with nothing to its left.
        assert!(
            rows.iter()
                .any(|r| r.starts_with(&format!(" {}", UNICODE.border.top_left))),
            "{rows:?}"
        );
        assert!(cursor.is_some(), "nowhere to type at 80x24");
        // And the header survived.
        assert!(rows.iter().any(|r| r.contains("Emma")), "{rows:?}");
    }

    #[test]
    fn short_windows_shed_the_status_border_and_then_the_subtitle() {
        let r = regions(Rect::new(0, 0, 100, 13), sidebar::width(100, false), 3);
        assert_eq!(r.status.height, 1, "under 14 rows the bar is one row");
        assert_eq!(r.header.height, 2, "13 rows still carries the subtitle");
        let r = regions(Rect::new(0, 0, 100, 11), sidebar::width(100, false), 3);
        assert_eq!(r.header.height, 1, "under 12 rows the header is one line");
        assert_eq!(r.rule.height, 0);
    }

    #[test]
    fn a_narrow_window_drops_the_main_border() {
        let r = regions(Rect::new(0, 0, 59, 20), 0, 3);
        assert!(!r.main_bordered);
        assert_eq!(r.chat.width, 59, "an absent border costs no columns");
    }

    // -----------------------------------------------------------------------
    // The dock
    // -----------------------------------------------------------------------

    #[test]
    fn the_dock_grows_for_the_menu_and_the_prompt_but_never_past_half_the_pane() {
        let mut v = view();
        assert_eq!(dock_height(&v, 30, None), 3);
        v.menu = Some(crate::term::menu::MenuView {
            rows: vec![("help".into(), "about".into()); 4],
            selected: 0,
            note: None,
        });
        assert_eq!(dock_height(&v, 30, None), 7);
        v.menu = None;
        v.prompt = Some(Prompt {
            title: "Approve Bash".into(),
            preview: (0..40).map(|i| format!("line {i}")).collect(),
            keys: vec![("y".into(), "yes".into())],
            question: "allow? ".into(),
        });
        assert_eq!(
            dock_height(&v, 30, None),
            15,
            "the prompt is capped at half"
        );
    }

    /// §4.6, in the new frame: the approval panel replaces the input box and
    /// no amount of transcript output can move it.
    #[test]
    fn a_pending_question_replaces_the_input_box_and_stays_on_screen() {
        let mut app = App::new((100, 30));
        let sk = skin();
        for i in 0..200 {
            app.push_block(vec![Line::raw(format!("output line {i}"))], &sk);
        }
        let mut v = view();
        v.prompt = Some(Prompt {
            title: "Approve Bash".into(),
            preview: vec!["$ rm -rf build/".into()],
            keys: vec![("y".into(), "yes".into()), ("n".into(), "no".into())],
            question: "allow? ".into(),
        });
        let (rows, cursor) = draw(&mut app, &v, 100, 30);
        let all = rows.join("\n");
        assert!(all.contains("Approve Bash"), "{all}");
        assert!(all.contains("rm -rf build/"), "{all}");
        assert!(all.contains(" y "), "{all}");
        assert!(
            !all.contains("> "),
            "the input box was drawn beside the question: {all}"
        );
        assert!(cursor.is_some());
    }

    // -----------------------------------------------------------------------
    // What this file draws itself
    // -----------------------------------------------------------------------

    #[test]
    fn the_header_carries_the_wordmark_the_version_and_the_real_directory() {
        let mut app = App::new((100, 30));
        let (rows, _) = draw(&mut app, &view(), 100, 30);
        let word = rows.iter().find(|r| r.contains("Emma")).expect("no header");
        assert!(
            word.contains(concat!("v", env!("CARGO_PKG_VERSION"))),
            "{word:?}"
        );
        assert!(
            rows.iter().any(|r| r.contains("Projects")),
            "the subtitle is not the real cwd: {rows:?}"
        );
    }

    /// The hint carries the keys; the chat pane carries the scrolled-behind
    /// indicator on its own last row. Both are asserted through the full
    /// render so the seam — this shell handing the pane a transcript wrapped
    /// at [`chat::message_width`] — is what is exercised.
    #[test]
    fn the_hint_names_the_keys_and_a_scrolled_reader_sees_the_pane_indicator() {
        let mut app = App::new((100, 30));
        let sk = skin();
        for i in 0..100 {
            app.push_block(vec![Line::raw(format!("line {i}"))], &sk);
        }
        let v = view();
        // Following: the ordinary hint, no indicator anywhere.
        let (rows, _) = draw(&mut app, &v, 100, 30);
        assert!(
            rows.iter().any(|r| r.contains("Ctrl-B sidebar")),
            "{rows:?}"
        );
        assert!(!rows.join("\n").contains("rows below"), "{rows:?}");
        // Scrolled: the chat pane's overlay appears, with a count and the
        // way back.
        app.transcript.scroll_up(50);
        let (rows, _) = draw(&mut app, &v, 100, 30);
        let indicator = rows
            .iter()
            .find(|r| r.contains("rows below"))
            .expect("no catch-up indicator anywhere on screen");
        assert!(indicator.contains("End"), "{indicator:?}");
    }

    #[test]
    fn the_working_hint_still_names_the_interrupt() {
        let mut app = App::new((100, 30));
        let mut v = view();
        v.mode = Mode::Working;
        let (rows, _) = draw(&mut app, &v, 100, 30);
        assert!(
            rows.iter()
                .any(|r| r.contains("Ctrl-C") && r.contains("runs next")),
            "{rows:?}"
        );
    }

    /// A window that is barely there must not panic — a resize can report
    /// anything mid-frame.
    #[test]
    fn degenerate_windows_are_survived() {
        let mut app = App::new((0, 0));
        for (w, h) in [(0u16, 0u16), (1, 1), (5, 3), (24, 8), (200, 2)] {
            let _ = draw(&mut app, &view(), w, h);
        }
    }

    /// New blocks are wrapped at the width they will be drawn at, including
    /// after a resize — the seam between `push_block` and `render`.
    #[test]
    fn a_resize_rewraps_the_retained_transcript() {
        let mut app = App::new((100, 30));
        let sk = skin();
        app.push_block(vec![Line::raw("x".repeat(90))], &sk);
        let before = app.transcript.height(&sk);
        let _ = draw(&mut app, &view(), 40, 20);
        assert!(
            app.transcript.height(&sk) > before,
            "narrowing the window did not rewrap the transcript"
        );
    }

    // -----------------------------------------------------------------------
    // The click
    //
    // The collapse defect this fixes was reported as "+ and - does not seem
    // to work": the owner aimed at the affordance with the mouse, and nothing
    // routed a click anywhere. The hit-test is here because only the layout
    // knows which cells were the sidebar's.
    // -----------------------------------------------------------------------

    /// A click on the SESSIONS header row — the row the `[-]` affordance is
    /// on — collapses the sidebar, and latches it as the user's choice.
    #[test]
    fn a_click_on_the_sessions_header_toggles_the_sidebar_and_latches() {
        let mut app = App::new((120, 30));
        let (rows, _) = draw(&mut app, &view(), 120, 30);
        assert!(
            rows.iter().any(|r| r.contains("SESSIONS")),
            "precondition: the sidebar is not even drawn: {rows:?}"
        );
        // The float puts the sidebar at (1,1); its header row is inside the
        // border, one row down.
        assert!(app.click(5, 2, 120), "the header click did not toggle");
        let (rows, _) = draw(&mut app, &view(), 120, 30);
        assert!(
            !rows.iter().any(|r| r.contains("SESSIONS")),
            "the sidebar is still drawn after a collapse click: {rows:?}"
        );
        // A user's click is a user's latch: widening the window must not
        // reopen what they closed — the §3.2 stickiness rule, via the mouse.
        let (rows, _) = draw(&mut app, &view(), 200, 40);
        assert!(
            !rows.iter().any(|r| r.contains("SESSIONS")),
            "the automatic rule overrode the user's click: {rows:?}"
        );
        // Collapsed, there is nothing on those cells to click.
        assert!(!app.click(5, 2, 200), "a hidden sidebar took a click");
    }

    /// Clicks anywhere else — the border row, a session row, the chat pane —
    /// toggle nothing. The one control gets the one row.
    #[test]
    fn a_click_anywhere_but_the_header_row_toggles_nothing() {
        let mut app = App::new((120, 30));
        let _ = draw(&mut app, &view(), 120, 30);
        for (x, y) in [
            (5u16, 1u16), // the sidebar's top border row
            (5, 3),       // the first session row
            (60, 2),      // the chat pane, same row as the header
            (0, 2),       // the float column left of the sidebar's border
        ] {
            assert!(!app.click(x, y, 120), "({x},{y}) toggled the sidebar");
        }
        let (rows, _) = draw(&mut app, &view(), 120, 30);
        assert!(rows.iter().any(|r| r.contains("SESSIONS")), "{rows:?}");
    }

    /// The user-expand half of the same latch, through the key path the click
    /// shares: expanded by hand below the auto-collapse threshold, the
    /// sidebar stays — the user insisted, the layout obeys.
    #[test]
    fn a_user_expand_below_the_threshold_survives_the_automatic_rule() {
        let mut app = App::new((80, 24));
        let (rows, _) = draw(&mut app, &view(), 80, 24);
        assert!(
            !rows.iter().any(|r| r.contains("SESSIONS")),
            "precondition: 80 columns should auto-collapse: {rows:?}"
        );
        app.toggle_sidebar(80);
        let (rows, _) = draw(&mut app, &view(), 80, 24);
        assert!(
            rows.iter().any(|r| r.contains("SESSIONS")),
            "the user's expand was overruled by the width rule: {rows:?}"
        );
    }

    // -----------------------------------------------------------------------
    // The TOOLS section
    // -----------------------------------------------------------------------

    /// The catalogue's entries land in the sidebar's row contract with the
    /// *real* binding in the key column: `Alt+<key>` chords, `/` for Search
    /// (the command menu the character already opens), and `n/a` where a tool
    /// cannot launch — never a bare letter, which is not a binding here.
    #[test]
    fn tool_rows_carry_the_real_chords_and_mark_the_unavailable() {
        let entries = vec![
            crate::usertools::Entry {
                tool: crate::usertools::Tool::Shell,
                label: "Shell".into(),
                key: 's',
                detail: "open a shell here".into(),
                available: true,
            },
            crate::usertools::Entry {
                tool: crate::usertools::Tool::Search,
                label: "Search".into(),
                key: '/',
                detail: "the command menu".into(),
                available: true,
            },
            crate::usertools::Entry {
                tool: crate::usertools::Tool::DataExplorer,
                label: "Data Explorer".into(),
                key: 'd',
                detail: "".into(),
                available: false,
            },
        ];
        let rows = tool_rows(&entries);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].name, "Shell");
        assert_eq!(rows[0].trailing, "Alt+s");
        assert_eq!(rows[1].trailing, "/");
        assert_eq!(
            rows[2].trailing, "n/a",
            "an unavailable tool showed a key that would do nothing"
        );
        // …and they reach the drawn TOOLS section through `set_tools`.
        let mut app = App::new((120, 30));
        app.set_tools(rows);
        let (drawn, _) = draw(&mut app, &view(), 120, 30);
        let all = drawn.join("\n");
        assert!(all.contains("Shell"), "{all}");
        assert!(all.contains("Alt+s"), "{all}");
    }

    /// The chord class is discoverable where the design says keys are
    /// discovered: the QUICK HELP table.
    #[test]
    fn quick_help_names_the_tool_chords() {
        let mut app = App::new((130, 40));
        // **Set the tools, because the real app does.** `App::new` seeds the
        // list with the slash commands as a placeholder and the shell replaces
        // it with the user-tool catalogue before the first paint. Drawing the
        // placeholder tests a state nobody sees — and it is length-sensitive:
        // adding two slash commands made the untouched sidebar tall enough to
        // push QUICK HELP off a 40-row window, which failed this test for a
        // reason that has nothing to do with what it asserts.
        app.set_tools(vec![sidebar::Row {
            name: "Shell".into(),
            trailing: "Alt+s".into(),
            selected: false,
        }]);
        let (rows, _) = draw(&mut app, &view(), 130, 40);
        assert!(
            rows.iter().any(|r| r.contains("Alt+key")),
            "the tool chords are not in QUICK HELP: {rows:?}"
        );
    }

    #[test]
    fn the_stem_is_the_last_real_component() {
        assert_eq!(stem("C:\\src\\emma"), "emma");
        assert_eq!(stem("/home/alan/emma/"), "emma");
        assert_eq!(stem("emma"), "emma");
    }
}
