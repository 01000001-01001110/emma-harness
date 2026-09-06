//! One composition, every window.
//!
//! The full-screen frame is a sidebar, a main region, and a bottom status
//! bar. Which *screen* occupies the main region varies — chat today, settings
//! behind `,`, whatever comes next — but the chrome around it does not, and
//! the owner's instruction is structural: every window reserves and renders
//! the sidebar and the bottom bar through this one seam. A screen composed
//! anywhere else is a bug, not a style.
//!
//! What lives here is exactly the shared arithmetic and the shared chrome:
//! the latch that decides whether the sidebar shows, the carve that turns a
//! window into rectangles, and [`compose`], which paints the chrome and hands
//! the main region to whichever screen is up. What a screen paints inside its
//! region is its own business; that it sits inside the chrome is not.
//!
//! Extracted from `app.rs`, where the settings screen had already grown its
//! own copy of the status-bar rendering and quietly stopped honouring a
//! configured status program — the drift this module exists to end.

use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Margin, Position, Rect};
use ratatui::widgets::{Block, Widget};

use super::palette::Role;
use super::view::{Mode, View};
use super::{sidebar, statusbar};

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
    let status_h = if area.height < SLIM_STATUS_ROWS { 1 } else { 3 };
    let [content, status] =
        Layout::vertical([Constraint::Min(0), Constraint::Length(status_h)]).areas(area);
    let [sidebar, main] =
        Layout::horizontal([Constraint::Length(sidebar_w), Constraint::Min(0)]).areas(content);
    let main_bordered = area.width >= UNBORDERED_COLS && main.width >= 3 && main.height >= 3;
    let inner = if main_bordered {
        main.inner(Margin::new(1, 1))
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
///
/// **`width` is what makes the input box grow with what is typed**, and it was
/// dropped by the TUI import: the incoming `layout.rs` took `(view, room)`
/// only, so a message longer than one line was hidden behind a three-row box.
/// The wrapping is `super::view::dock_rows`' — the same function the box paints
/// with, so the height and the paint cannot disagree. `None` is "the caller
/// does not know the width yet" and answers with the old three-row floor.
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

// region: The seam
// ---------------------------------------------------------------------------
// The seam
// ---------------------------------------------------------------------------

/// Compose one window: sidebar, main border, the screen, the bottom bar.
///
/// The screen gets the carved [`Regions`] and paints only the main region's
/// interior — header through hint, or all of it as one pane. Everything
/// around it is painted here, unconditionally, which is what makes "this
/// window forgot its status bar" an impossible state rather than a review
/// comment.
///
/// The bar's border belongs to this seam — `statusbar::render` paints one row
/// of cells into whatever row it is handed, which is what lets the border be
/// shed below [`SLIM_STATUS_ROWS`] without the widget knowing the window's
/// height. A configured status program replaces the built-in cells' row on
/// *every* screen, inside the same border — the same trade it makes inline.
pub fn compose(
    area: Rect,
    buf: &mut Buffer,
    view: &View,
    bar: &statusbar::Bar,
    side: &sidebar::State,
    dock_h: u16,
    screen: impl FnOnce(&Regions, &mut Buffer) -> Option<Position>,
) -> Option<Position> {
    if area.width == 0 || area.height == 0 {
        return None;
    }
    let skin = &view.skin;
    let r = regions(area, sidebar::width(area.width, side.collapsed), dock_h);

    sidebar::render(r.sidebar, buf, side, skin);

    if r.main_bordered {
        Block::bordered()
            .border_set(skin.glyphs.border)
            .border_style(skin.palette.dim())
            .render(r.main, buf);
    }

    let cursor = screen(&r, buf);

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
        // A configured status program replaces the built-in cells' row — the
        // same trade it makes inline, inside the same border the built-in bar
        // would have had.
        Some(text) => {
            super::statusline::to_line(text, view.status_padding, skin.palette.style(Role::Text))
                .render(status_row, buf);
        }
        None => statusbar::render(status_row, buf, bar, skin),
    }
    cursor
}

/// Fill the bar from what the shell measured. The cells map to real
/// measurements or they do not appear: the meter contract encodes absence as
/// a non-positive cap, so an unmeasured `context`/`spend` becomes `(0, 0)` —
/// never `(0, real_cap)`, which would draw an invented "0% of the budget"
/// before the first model call reports. The ↑/↓ split is `Status`'s running
/// raw totals, always `Some`: zero traffic is a measurement, not an unknown.
pub fn bar_for(view: &View) -> statusbar::Bar {
    let status = &view.status;
    statusbar::Bar {
        // `ASSIST` is the mock's word for idle-and-ready, adopted by the
        // owner's fiat (mock rulings, 2026-08-26); a running goal keeps
        // naming its working state.
        mode: match view.mode {
            // The live posture, not a constant: /mode switches it and the bar
            // must say what the gate is enforcing at draw time.
            Mode::Idle => crate::approval::current_mode_label().to_string(),
            Mode::Working => "WORKING".to_string(),
        },
        model: status.model.clone(),
        // **Two of the incoming fields are not set, because this tree's
        // `statusbar` has nowhere to put them.** `idle` gated the mode cell's
        // diamond on the ready state; here the diamond is always drawn — it
        // brackets the posture rather than badging it, which is the argument in
        // `statusbar`'s own glyph table. `environment` prefixed the ENV cell
        // with the mock's `local`; here the cell is the working directory's
        // last component and nothing else. Both are cosmetic and both are a
        // real difference between the two screens: an owner comparing this
        // against the mock will see one diamond too many and one word too few.
        env: super::app::stem(&status.cwd),
        ctx_used: status.context.map(|(u, _)| u).unwrap_or(0),
        ctx_max: status.context.map(|(_, c)| c).unwrap_or(0),
        // The TOKENS cell is the **session's** running story, not the running
        // goal's — three session-scoped fields, none of which a new goal
        // clears. `Status::spend` is the goal's budget meter and belongs to
        // the inline row instead; feeding it here is what made the cell reset
        // on every submission (2026-08-26). `total_max` stays the goal's cap
        // because the cell draws no cap at all: there is no session budget,
        // and inventing one to fill the field would put a number on screen
        // that nothing enforces.
        total_used: status.session_tokens,
        total_max: status.spend.map(|(_, c)| c).unwrap_or(0),
        up: Some(status.up),
        down: Some(status.down),
        elapsed: status.elapsed,
    }
}

// endregion: The seam

#[cfg(test)]
mod tests {
    use super::super::palette::{Level, Palette};
    use super::super::render::{Skin, UNICODE};
    use super::*;

    fn skin() -> Skin {
        Skin::new(Palette::new(Level::Truecolor), UNICODE)
    }

    fn view() -> View {
        let mut v = View::new(skin());
        v.status.model = "claude-opus-4".into();
        v.status.cwd = "/tmp/emma".into();
        v
    }

    fn bar() -> statusbar::Bar {
        statusbar::Bar {
            mode: "ASSIST".into(),
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

    fn rows_of(buf: &Buffer, w: u16, h: u16) -> Vec<String> {
        (0..h)
            .map(|y| {
                (0..w)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect()
    }

    /// Any screen composed through this seam gets the chrome, whatever it
    /// paints — here, a screen that paints one marker word and nothing else.
    #[test]
    fn any_screen_composed_here_gets_the_sidebar_and_the_status_bar() {
        let v = view();
        let area = Rect::new(0, 0, 120, 40);
        let mut buf = Buffer::empty(area);
        let side = sidebar::State {
            sessions: vec![],
            commands: vec![],
            help: vec![("q".into(), "quit".into())],
            today: None,
            collapsed: false,
        };
        compose(area, &mut buf, &v, &bar(), &side, 3, |r, buf| {
            buf.set_string(
                r.chat.x,
                r.chat.y,
                "OCCUPANT",
                ratatui::style::Style::default(),
            );
            None
        });
        let rows = rows_of(&buf, 120, 40);
        let all = rows.join("\n");
        for section in ["SESSIONS", "TOOLS", "QUICK HELP"] {
            assert!(all.contains(section), "sidebar lacks {section}");
        }
        assert!(all.contains("OCCUPANT"), "the screen was not painted");
        let bar_rows = rows[37..].join("\n");
        for cell in ["MODE", "ENV"] {
            assert!(
                bar_rows.contains(cell),
                "bottom bar lacks {cell}: {bar_rows:?}"
            );
        }
    }

    /// The ↑/↓ split is live measurement, never `None`: two calls' raw
    /// input/output totals fold into `Status` and come out of [`bar_for`] as
    /// `Some` sums, and a session that has not spent anything yet reports a
    /// measured zero — the mock's shape, with honest numbers.
    #[test]
    fn the_bar_carries_the_measured_token_split_never_none() {
        let mut v = view();
        v.status.record_call(1_000, 200);
        v.status.record_call(2_000, 300);
        let b = bar_for(&v);
        assert_eq!(b.up, Some(3_000), "cumulative raw input");
        assert_eq!(b.down, Some(500), "cumulative raw output");

        let fresh = view();
        let b = bar_for(&fresh);
        assert_eq!(
            b.up,
            Some(0),
            "no session yet is a measured zero, not unknown"
        );
        assert_eq!(b.down, Some(0));
    }

    /// The reset the owner saw, pinned at the seam that draws it: after a new
    /// goal opens, the bar's whole TOKENS cell still reads the session, while
    /// the goal-scoped meter behind the inline row starts empty.
    #[test]
    fn a_new_goal_does_not_clear_the_bars_tokens_cell() {
        let mut v = view();
        v.status.record_call(8_128, 4_714);
        v.status.record_spend(12_842, 500_000);

        v.status.goal_started();

        let b = bar_for(&v);
        assert_eq!(
            b.total_used, 12_842,
            "the session's bill is still on the bar"
        );
        assert_eq!(b.up, Some(8_128), "and so is the input half of the split");
        assert_eq!(b.down, Some(4_714), "and the output half");
        assert_eq!(
            v.status.spend, None,
            "while the per-goal budget meter is back to empty, which is its job"
        );
    }

    /// The collapsed sidebar costs zero columns and the chrome survives.
    #[test]
    fn a_collapsed_sidebar_leaves_the_bar_standing() {
        let v = view();
        let area = Rect::new(0, 0, 80, 20);
        let mut buf = Buffer::empty(area);
        let side = sidebar::State {
            sessions: vec![],
            commands: vec![],
            help: vec![],
            today: None,
            collapsed: true,
        };
        compose(area, &mut buf, &v, &bar(), &side, 3, |_, _| None);
        let rows = rows_of(&buf, 80, 20);
        assert!(
            !rows.join("\n").contains("SESSIONS"),
            "a collapsed sidebar still painted"
        );
        let bar_rows = rows[17..].join("\n");
        assert!(
            bar_rows.contains("MODE"),
            "bottom bar missing: {bar_rows:?}"
        );
    }
}
