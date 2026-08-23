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
pub fn dock_height(view: &View, room: u16) -> u16 {
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
            None => 3.min(ceiling),
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
        let r = regions(area, sb_w, dock_height(view, area.height));
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
        let label_w = rows.iter().map(|(l, _, _)| l.len()).max().unwrap_or(0) + 2;
        for (label, value, why) in &rows {
            if y >= end {
                break;
            }
            let line = Line::from(vec![
                Span::styled(format!("{label:<label_w$}"), skin.palette.style(Role::Text)),
                Span::styled(value.clone(), skin.palette.bold(Role::Accent)),
                Span::styled(format!("   {why}"), skin.palette.dim()),
            ]);
            line.render(Rect::new(r.chat.x, y, r.chat.width, 1), buf);
            y = y.saturating_add(1);
        }

        y = y.saturating_add(1);
        if y < end {
            Line::from(Span::styled("Keys", skin.palette.bold(Role::Accent)))
                .render(Rect::new(r.chat.x, y, r.chat.width, 1), buf);
            y = y.saturating_add(1);
        }
        for (k, what) in keymap() {
            if y >= end {
                break;
            }
            Line::from(vec![
                Span::styled(format!("{k:<label_w$}"), skin.palette.dim()),
                Span::styled(what, skin.palette.style(Role::Text)),
            ])
            .render(Rect::new(r.chat.x, y, r.chat.width, 1), buf);
            y = y.saturating_add(1);
        }

        y = y.saturating_add(1);
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
            Line::from(Span::styled(line, skin.palette.dim()))
                .render(Rect::new(r.chat.x, y, r.chat.width, 1), buf);
            y = y.saturating_add(1);
        }
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
fn keymap() -> Vec<(String, String)> {
    [
        ("/", "command menu"),
        ("Enter", "send"),
        ("Esc", "close menu"),
        ("PgUp/PgDn", "scroll"),
        ("Ctrl+Up/Dn", "scroll a row"),
        ("Home/End", "top / tail (empty box)"),
        ("Ctrl+B", "toggle sidebar"),
        ("Alt+key", "launch tool"),
        ("Ctrl+C", "interrupt"),
        ("Ctrl+D", "quit"),
        ("Shift+drag", "select text"),
    ]
    .into_iter()
    .map(|(k, d)| (k.to_string(), d.to_string()))
    .collect()
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
            dock_height(&view(), 30),
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
        assert_eq!(dock_height(&v, 30), 3);
        v.menu = Some(crate::term::menu::MenuView {
            rows: vec![("help".into(), "about".into()); 4],
            selected: 0,
            note: None,
        });
        assert_eq!(dock_height(&v, 30), 7);
        v.menu = None;
        v.prompt = Some(Prompt {
            title: "Approve Bash".into(),
            preview: (0..40).map(|i| format!("line {i}")).collect(),
            keys: vec![("y".into(), "yes".into())],
            question: "allow? ".into(),
        });
        assert_eq!(dock_height(&v, 30), 15, "the prompt is capped at half");
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
