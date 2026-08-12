//! The full-screen frame: the layout that owns the whole window, and the state
//! that survives between paints.
//!
//! This is stage 2 of `notes/design-tui-fullscreen.md` — the point of no
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
//! proportions are the mockup's, as pixel-sampled in the design note §1 — the
//! prose description of that image was wrong in five recorded places, so the
//! numbers here cite the sampled table, not the prose.
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
#[derive(Debug)]
pub struct App {
    pub transcript: Transcript,
    latch: Latch,
    side: sidebar::State,
    /// The column entries are wrapped to: [`chat::message_width`] of the chat
    /// pane, so the wrapper and the painter agree on where the column ends —
    /// the one number the chat module insists both sides derive from it.
    wrap_width: u16,
    /// The chat pane's height at the last layout; less one row, a page.
    chat_height: u16,
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
            latch,
            side: sidebar::State {
                sessions: Vec::new(),
                commands: builtin_rows(),
                help: keymap(),
                collapsed: false,
            },
            wrap_width: chat::message_width(r.chat.width.max(1)),
            chat_height: r.chat.height.max(1),
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

        sidebar::render(r.sidebar, buf, &self.side, skin);

        if r.main_bordered {
            Block::bordered()
                .border_set(skin.glyphs.border)
                .border_style(skin.palette.dim())
                .render(r.main, buf);
        }
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
        cursor
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
        // The main pane is everything the sidebar left, and the chat area is
        // inside its border.
        assert_eq!(r.main.width, 120 - 28);
        assert_eq!(r.chat.width, 120 - 28 - 2);
        // Bottom-up inside the pane: chat, dock, hint.
        assert!(r.chat.y > r.header.y);
        assert_eq!(r.dock.y, r.chat.bottom());
        assert_eq!(r.hint.y, r.dock.bottom());
        assert_eq!(r.status.y, 27);
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
        // No sidebar columns: the main pane's border is at column zero.
        assert!(
            rows.iter().any(|r| r.starts_with(UNICODE.border.top_left)),
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

    #[test]
    fn the_stem_is_the_last_real_component() {
        assert_eq!(stem("C:\\src\\emma"), "emma");
        assert_eq!(stem("/home/alan/emma/"), "emma");
        assert_eq!(stem("emma"), "emma");
    }
}
