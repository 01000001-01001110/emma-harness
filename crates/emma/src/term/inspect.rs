//! The Inspect Run page: the owner's mock, cell for cell.
//!
//! The mock is the acceptance criterion (the Inspect page design), the
//! standing Settings/Memory rule: a head block, a seven-cell header info
//! strip, STEP TIMELINE, RUN METADATA, TOOL CALLS, ARTIFACTS and EVENT LOG
//! cards, and the page's own input bar, with every label, glyph, footer and
//! affordance from the mock.
//!
//! The one sanctioned divergence is data. The harness this page describes is
//! aspirational (the harness plan), so everything renders from an
//! [`InspectView`]: a populated view reproduces the mock exactly (the tests
//! pin that now, with the mock's sample data), and the live view starts with
//! no run at all. `run: None` renders the head plus one honest dim line —
//! [`EMPTY_PAGE`] — and nothing else, because every cell and card on this
//! page describes one run and empty chrome would claim a run exists. The
//! input bar is withheld too: its commands all operate on a run.
//!
//! Selection is render-only state, not keys (keys are integration's):
//! `selected_step` wraps its row in an accent border with a `▷ ` prefix (the
//! mock's dress), and `selected_tool` wears the sidebar's chip band.
//!
//! Pure rendering, like [`super::settings`] and [`super::memory`]: the shell
//! owns whether the page is open, and the global bottom-bar override the mock
//! shows (MODE INSPECT / RUN / PROGRESS / CTX) is integration's, not drawn
//! here. All width arithmetic is in display columns via [`cols`]/[`fit`], and
//! no row ever writes past its card's inner width: the trailing column
//! survives and the text truncates, the sidebar's ruling.

use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Widget};

use super::palette::Role;
use super::render::{cols, fit, Skin, ASCII};

// region: State
// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

/// The color of the STATUS cell's dot and word. The mock's one state is
/// `● Running` in green; the enum exists so a failed run can never wear it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RunHealth {
    #[default]
    Ok,
    Warn,
    Err,
}

/// One STEP TIMELINE row's state. Decides the right side's glyph, word and
/// role; a pending step's time is always the dash, whatever the field says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepStatus {
    Completed,
    Running,
    Pending,
}

/// One row of the STEP TIMELINE card.
#[derive(Debug, Clone)]
pub struct Step {
    pub name: String,
    pub status: StepStatus,
    /// Elapsed text (`1.2s`), caller-formatted. Ignored for Pending.
    pub time: String,
}

/// One TOOL CALLS row's state. The mock shows `✓ Allowed` and `⏳ Pending`;
/// real history also holds calls that ran and failed, and calls a hook or a
/// human refused — rendering either as Allowed would be a false claim, so
/// they have their own words (harness-live, 2026-08-26).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolStatus {
    Allowed,
    Pending,
    /// The tool ran and failed.
    Failed,
    /// A hook or a human refused it; it never ran.
    Denied,
}

/// One row of the TOOL CALLS / APPROVALS card.
#[derive(Debug, Clone)]
pub struct ToolCall {
    pub tool: String,
    pub request: String,
    pub status: ToolStatus,
    /// Elapsed text; empty renders the dash, like a pending step.
    pub time: String,
}

/// One row of the ARTIFACTS / OUTPUT card.
#[derive(Debug, Clone)]
pub struct ArtifactRow {
    pub kind: String,
    pub name: String,
    pub size: String,
    pub modified: String,
}

/// An EVENT LOG row's level: WARN wears [`Role::Warn`], DEBUG is dim, INFO is
/// plain text. ERROR ([`Role::Err`]) is not in the mock — real session logs
/// record harness failures, and WARN would understate one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Info,
    Warn,
    Debug,
    Error,
}

/// One row of the EVENT LOG card.
#[derive(Debug, Clone)]
pub struct Event {
    pub time: String,
    pub level: Level,
    pub text: String,
}

/// One run, fully described — the header strip, the five cards. Every field
/// is caller-formatted display text; this layer draws and never computes.
#[derive(Debug, Clone, Default)]
pub struct RunView {
    // -- the header info strip, cell by cell --------------------------------
    pub name: String,
    pub id: String,
    /// The STATUS word (`Running`), colored by `health`.
    pub status: String,
    pub health: RunHealth,
    /// Whole percent; drives the STATUS cell's mini-gauge.
    pub progress_pct: u8,
    pub started: String,
    pub started_ago: String,
    pub duration: String,
    pub eta: String,
    pub model: String,
    /// The MODEL cell's dim line (`ctx 18,432 (68%)`).
    pub context: String,
    pub workspace: String,
    /// The WORKSPACE cell's dim line, after the branch glyph (`main @ 3f7c9d2`).
    pub branch: String,
    pub agent: String,
    pub agent_runtime: String,

    // -- STEP TIMELINE -------------------------------------------------------
    pub steps: Vec<Step>,
    pub selected_step: usize,
    /// The header's done count, rendered verbatim. The mock says `12 / 18`
    /// while its rows show 7 ✓ + 1 ↻ — the inconsistency is the mock's, and
    /// this is the settings sample-value rule, not a derivation.
    pub steps_done: u64,

    // -- TOOL CALLS / APPROVALS ---------------------------------------------
    /// The header's `Auto-approve:` value.
    pub auto_approve: String,
    pub tools: Vec<ToolCall>,
    pub selected_tool: Option<usize>,

    // -- RUN METADATA --------------------------------------------------------
    /// Caller-supplied `(label, value)` pairs, in the mock's order. Pairs
    /// rather than typed fields: the backend that will feed them does not
    /// exist yet (design note).
    pub metadata: Vec<(String, String)>,

    // -- ARTIFACTS / OUTPUT --------------------------------------------------
    pub artifacts: Vec<ArtifactRow>,

    // -- EVENT LOG -----------------------------------------------------------
    pub events: Vec<Event>,
}

/// What the page shows. `run: None` is the live launch state — no harness
/// run exists to inspect, and the page says so in one line.
#[derive(Debug, Clone, Default)]
pub struct InspectView {
    /// The running binary's version, `v`-prefixed — the mock's top-right corner.
    pub version: String,
    pub run: Option<RunView>,
}

/// The subtitle under the title, verbatim from the mock.
pub const SUBTITLE: &str = "Deep inspection of a single harness execution";

/// The whole-page honest empty state (design note: no cards, no input bar).
pub const EMPTY_PAGE: &str = "No run selected — [b] back to runs";

/// The strip's seven labels, in the mock's order. The transcription says six
/// cells and lists seven; all seven render (design note).
pub const STRIP_LABELS: [&str; 7] = [
    "RUN",
    "STATUS",
    "STARTED",
    "DURATION",
    "MODEL",
    "WORKSPACE",
    "AGENT",
];

/// The card footers, verbatim from the mock.
pub const STEPS_FOOTER: &str = "[↑/↓] Select  [Enter] Focus  [e] Expand  [c] Collapse All";
pub const TOOLS_FOOTER: &str = "[t] Filter  [Enter] Inspect  [a] Approve  [d] Deny  [o] Open";
pub const ARTIFACTS_FOOTER: &str = "[o] Open  [v] View  [d] Diff  [y] Copy Path";

/// The input bar's command pairs, verbatim from the mock.
pub const BAR_COMMANDS: [(&str, &str); 5] = [
    ("e", "expand step"),
    ("t", "open trace"),
    ("d", "view diff"),
    ("x", "export run"),
    ("b", "back to runs"),
];

// endregion: State

// region: Rendering
// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

/// The page's own glyphs, one ASCII fallback each — the `render::ASCII`
/// split, extended for shapes that file does not carry.
struct PageGlyphs {
    dot: &'static str,
    check: &'static str,
    cross: &'static str,
    running: &'static str,
    hourglass: &'static str,
    selected: &'static str,
    branch: &'static str,
    arrow: &'static str,
    full: &'static str,
    empty: &'static str,
    dash: &'static str,
}

fn page_glyphs(skin: &Skin) -> PageGlyphs {
    if skin.glyphs == ASCII {
        PageGlyphs {
            dot: "*",
            check: "+",
            cross: "x",
            running: "~",
            hourglass: "?",
            selected: ">",
            branch: "^",
            arrow: "->",
            full: "#",
            empty: "-",
            dash: "-",
        }
    } else {
        PageGlyphs {
            dot: "●",
            check: "✓",
            cross: "✗",
            running: "↻",
            hourglass: "⏳",
            selected: "▷",
            branch: "⎇",
            arrow: "→",
            full: "█",
            empty: "░",
            dash: "–",
        }
    }
}

/// The honest empty-state string carries an em dash and the steps footer the
/// arrow keys; a legacy code page gets ASCII stand-ins instead.
fn honest(text: &str, ascii: bool) -> String {
    if ascii {
        text.replace('—', "-").replace("↑/↓", "Up/Dn")
    } else {
        text.to_string()
    }
}

/// Draw the whole page into `area`. Too small an area draws what fits from
/// the top, clipped whole-row like every other widget.
pub fn render(area: Rect, buf: &mut Buffer, v: &InspectView, skin: &Skin) {
    if area.width < 4 || area.height < 2 {
        return;
    }
    let w = usize::from(area.width);
    let g = page_glyphs(skin);
    // The input bar is reserved from the bottom before anything above it is
    // measured, so a short terminal loses card rows rather than the one
    // control the page is driven from. Everything else lays out inside
    // `bottom`, which is the screen minus that reservation.
    let bar_h = if area.height >= 12 { 3 } else { 0 };
    let bottom = area.y + area.height - bar_h;
    let mut y = area.y;

    // The head: version in the corner, the title, the subtitle, a rule.
    // "Very large" is not a thing a terminal cell can do; one bold accent row
    // is this repository's standing substitute (settings design Q8).
    let version = fit(&v.version, w, skin.glyphs.ellipsis);
    let head = [
        Line::from(vec![
            Span::raw(" ".repeat(w.saturating_sub(cols(&version)))),
            Span::styled(version, skin.palette.dim()),
        ]),
        Line::from(Span::styled(
            fit("Inspect Run", w, skin.glyphs.ellipsis),
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

    // No run: the whole-page honest empty state, one dim line under a row of
    // air. No cards, no strip, no input bar — they all describe a run.
    let Some(r) = &v.run else {
        y += 1;
        if y < bottom {
            let ascii = skin.glyphs == ASCII;
            let line = Line::from(Span::styled(
                fit(&honest(EMPTY_PAGE, ascii), w, skin.glyphs.ellipsis),
                skin.palette.dim(),
            ));
            buf.set_line(area.x, y, &line, area.width);
        }
        return;
    };

    // The header info strip: seven bordered cells, one band.
    let strip_h = 5.min(bottom.saturating_sub(y));
    if strip_h >= 3 {
        render_strip(Rect::new(area.x, y, area.width, strip_h), buf, r, skin, &g);
        y += strip_h;
    }

    // The grid columns, one column of air between them.
    let grid = Rect::new(area.x, y.min(bottom), area.width, bottom.saturating_sub(y));
    let [lc, _, rc] = Layout::horizontal([
        Constraint::Fill(1),
        Constraint::Length(1),
        Constraint::Fill(1),
    ])
    .areas(grid);

    // Top pair: STEP TIMELINE | RUN METADATA. The timeline needs its rows,
    // two extra for the selection border, the footer, and the card border.
    let steps_h = r.steps.len().max(1) as u16 + 2 + 1 + 3;
    let meta_h = r.metadata.len().max(1) as u16 + 3;
    let h1 = steps_h.max(meta_h).min(bottom.saturating_sub(y));
    if h1 >= 3 {
        render_steps(Rect::new(lc.x, y, lc.width, h1), buf, r, skin, &g);
        render_metadata(Rect::new(rc.x, y, rc.width, h1), buf, r, skin);
        y += h1;
    }

    // Second pair: TOOL CALLS | ARTIFACTS. Column header + rows + footer is
    // the floor; the pair then absorbs whatever height the event log and the
    // input bar leave over, so a tall terminal fills instead of stopping
    // partway down the screen (owner report: these cards must size to the
    // screen, not to their row count).
    let tools_h = r.tools.len().max(1) as u16 + 1 + 1 + 3;
    let arts_h = r.artifacts.len().max(1) as u16 + 1 + 1 + 3;
    let ev_wants = r.events.len().max(1) as u16 + 3;
    let room = bottom.saturating_sub(y).saturating_sub(ev_wants);
    let h2 = tools_h.max(arts_h).max(room).min(bottom.saturating_sub(y));
    if h2 >= 3 {
        render_tools(Rect::new(lc.x, y, lc.width, h2), buf, r, skin, &g);
        render_artifacts(Rect::new(rc.x, y, rc.width, h2), buf, r, skin);
        y += h2;
    }

    // EVENT LOG, full width.
    let ev_h = (r.events.len().max(1) as u16 + 3).min(bottom.saturating_sub(y));
    if ev_h >= 3 {
        render_events(Rect::new(area.x, y, area.width, ev_h), buf, r, skin, &g);
    }

    // The page input bar, on the rows reserved for it at the bottom.
    if bar_h >= 3 {
        render_bar(Rect::new(area.x, bottom, area.width, bar_h), buf, skin);
    }
}

// -- the header info strip ---------------------------------------------------

/// A strip cell's dim-accent label style.
fn label_style(skin: &Skin) -> Style {
    skin.palette.style(Role::Accent).add_modifier(Modifier::DIM)
}

/// The seven cells. Weights 2/2/2/2/3/3/2: MODEL and WORKSPACE carry the
/// longest values in the mock and get the wider cells (design note).
fn render_strip(area: Rect, buf: &mut Buffer, r: &RunView, skin: &Skin, g: &PageGlyphs) {
    let cells: [Rect; 7] = Layout::horizontal([
        Constraint::Fill(2),
        Constraint::Fill(2),
        Constraint::Fill(2),
        Constraint::Fill(2),
        Constraint::Fill(3),
        Constraint::Fill(3),
        Constraint::Fill(2),
    ])
    .areas(area);
    let accent = skin.palette.style(Role::Accent);
    let dim = skin.palette.dim();
    let text = skin.palette.style(Role::Text);
    let ok = match r.health {
        RunHealth::Ok => skin.palette.style(Role::Ok),
        RunHealth::Warn => skin.palette.style(Role::Warn),
        RunHealth::Err => skin.palette.style(Role::Err),
    };
    // The STATUS cell's mini-gauge: the status bar's idiom, ten segments,
    // ceil fill capped at nine below 100%.
    let pct = u64::from(r.progress_pct.min(100));
    let filled = if pct >= 100 {
        10
    } else {
        (pct as usize * 10).div_ceil(100).min(9)
    };
    let mut gauge: Vec<Span<'static>> = Vec::new();
    if filled > 0 {
        gauge.push(Span::styled(g.full.repeat(filled), accent));
    }
    if filled < 10 {
        gauge.push(Span::styled(g.empty.repeat(10 - filled), dim));
    }
    gauge.push(Span::styled(format!(" {pct}%"), accent));
    let bodies: [[Vec<Span<'static>>; 2]; 7] = [
        [
            vec![Span::styled(
                r.name.clone(),
                text.add_modifier(Modifier::BOLD),
            )],
            vec![Span::styled(r.id.clone(), dim)],
        ],
        [
            vec![Span::styled(format!("{} {}", g.dot, r.status), ok)],
            gauge,
        ],
        [
            vec![Span::styled(r.started.clone(), text)],
            vec![Span::styled(r.started_ago.clone(), dim)],
        ],
        [
            vec![Span::styled(r.duration.clone(), text)],
            vec![Span::styled(r.eta.clone(), dim)],
        ],
        [
            vec![Span::styled(r.model.clone(), text)],
            vec![Span::styled(r.context.clone(), dim)],
        ],
        [
            vec![Span::styled(r.workspace.clone(), text)],
            // No branch claim, no dangling glyph: the live adapter has no
            // branch record to back one (harness-live).
            if r.branch.is_empty() {
                Vec::new()
            } else {
                vec![Span::styled(format!("{} {}", g.branch, r.branch), dim)]
            },
        ],
        [
            vec![Span::styled(r.agent.clone(), text)],
            vec![Span::styled(r.agent_runtime.clone(), dim)],
        ],
    ];
    for (i, body) in bodies.into_iter().enumerate() {
        let block = Block::bordered()
            .border_set(skin.glyphs.border)
            .border_style(dim);
        let inner = block.inner(cells[i]);
        block.render(cells[i], buf);
        if inner.width == 0 {
            continue;
        }
        let w = usize::from(inner.width);
        let label = Line::from(Span::styled(
            clip(STRIP_LABELS[i], w, skin),
            label_style(skin),
        ));
        set_row(inner, 0, buf, label);
        for (j, spans) in body.into_iter().enumerate() {
            set_row(inner, j as u16 + 1, buf, clipped_line(spans, w, skin));
        }
    }
}

// -- the cards ---------------------------------------------------------------

/// One bordered card: dim border, then the header line (left bold accent,
/// right the card's trailing spans). Returns the content area under the
/// header.
fn card_frame(
    area: Rect,
    buf: &mut Buffer,
    skin: &Skin,
    left: &str,
    right: Vec<Span<'static>>,
) -> Rect {
    let block = Block::bordered()
        .border_set(skin.glyphs.border)
        .border_style(skin.palette.dim());
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

/// A card footer, sunk to the last inner row, dim — the settings description
/// ruling.
fn sink_footer(content: Rect, buf: &mut Buffer, used: u16, text: &str, skin: &Skin) {
    if content.height > used {
        let line = Line::from(Span::styled(
            clip(
                &honest(text, skin.glyphs == ASCII),
                usize::from(content.width),
                skin,
            ),
            skin.palette.dim(),
        ));
        set_row(content, content.height - 1, buf, line);
    }
}

fn render_steps(area: Rect, buf: &mut Buffer, r: &RunView, skin: &Skin, g: &PageGlyphs) {
    if area.width < 8 || area.height < 3 {
        return;
    }
    let right = vec![Span::styled(
        format!("{} / {} steps   Expand: [e]", r.steps_done, r.steps.len()),
        skin.palette.dim(),
    )];
    let content = card_frame(
        area,
        buf,
        skin,
        "STEP TIMELINE / EXECUTION BREAKDOWN",
        right,
    );
    let w = usize::from(content.width);
    let dim = skin.palette.dim();
    let mut row = 0u16;
    for (i, step) in r.steps.iter().enumerate() {
        let selected = i == r.selected_step;
        // The selected row's dress is the mock's: its own accent border.
        let inner_w = if selected { w.saturating_sub(2) } else { w };
        let (word, style, time) = match step.status {
            StepStatus::Completed => (
                format!("{} Completed", g.check),
                skin.palette.style(Role::Ok),
                step.time.clone(),
            ),
            StepStatus::Running => (
                format!("{} Running", g.running),
                skin.palette.style(Role::Accent),
                step.time.clone(),
            ),
            StepStatus::Pending => (format!("{} Pending", g.dot), dim, g.dash.to_string()),
        };
        let prefix = if selected {
            format!("{} ", g.selected)
        } else {
            String::new()
        };
        let name_style = if step.status == StepStatus::Pending {
            dim
        } else {
            skin.palette.style(Role::Text)
        };
        let line = lr(
            vec![
                Span::styled(format!("{prefix}{:02} ", i + 1), dim),
                Span::styled(step.name.clone(), name_style),
            ],
            vec![
                Span::styled(word, style),
                Span::styled(format!("  {time}"), dim),
            ],
            inner_w,
            skin,
        );
        if selected {
            if row + 3 <= content.height {
                let sel = Rect::new(content.x, content.y + row, content.width, 3);
                let block = Block::bordered()
                    .border_set(skin.glyphs.border)
                    .border_style(skin.palette.style(Role::Accent));
                let sel_inner = block.inner(sel);
                block.render(sel, buf);
                buf.set_line(sel_inner.x, sel_inner.y, &line, sel_inner.width);
            }
            row += 3;
        } else {
            set_row(content, row, buf, line);
            row += 1;
        }
        if row >= content.height {
            break;
        }
    }
    sink_footer(content, buf, row, STEPS_FOOTER, skin);
}

fn render_metadata(area: Rect, buf: &mut Buffer, r: &RunView, skin: &Skin) {
    if area.width < 4 || area.height < 3 {
        return;
    }
    let content = card_frame(area, buf, skin, "RUN METADATA", Vec::new());
    let w = usize::from(content.width);
    for (i, (label, value)) in r.metadata.iter().enumerate() {
        let line = lr(
            vec![Span::styled(label.clone(), skin.palette.style(Role::Text))],
            vec![Span::styled(
                value.clone(),
                skin.palette.style(Role::Accent),
            )],
            w,
            skin,
        );
        set_row(content, i as u16, buf, line);
    }
}

/// Pad `text` with trailing spaces to `w` display columns (clipping first).
fn pad(text: &str, w: usize, skin: &Skin) -> String {
    let t = clip(text, w, skin);
    let missing = w.saturating_sub(cols(&t));
    format!("{t}{}", " ".repeat(missing))
}

/// Pad `text` with leading spaces to `w` display columns (clipping first).
fn pad_left(text: &str, w: usize, skin: &Skin) -> String {
    let t = clip(text, w, skin);
    let missing = w.saturating_sub(cols(&t));
    format!("{}{t}", " ".repeat(missing))
}

const TOOL_W: usize = 11;
const STATUS_W: usize = 11;
const TIME_W: usize = 5;

fn render_tools(area: Rect, buf: &mut Buffer, r: &RunView, skin: &Skin, g: &PageGlyphs) {
    if area.width < 12 || area.height < 3 {
        return;
    }
    let right = vec![Span::styled(
        format!("Auto-approve: {}", r.auto_approve),
        skin.palette.dim(),
    )];
    let content = card_frame(area, buf, skin, "TOOL CALLS / APPROVALS", right);
    let w = usize::from(content.width);
    let dim = skin.palette.dim();
    // The column header row.
    let header = lr(
        vec![Span::styled(
            format!("{}REQUEST", pad("TOOL", TOOL_W, skin)),
            dim,
        )],
        vec![Span::styled(
            format!(
                "{}  {}",
                pad("STATUS", STATUS_W, skin),
                pad_left("TIME", TIME_W, skin)
            ),
            dim,
        )],
        w,
        skin,
    );
    set_row(content, 0, buf, header);
    for (i, call) in r.tools.iter().enumerate() {
        let (word, style) = match call.status {
            ToolStatus::Allowed => (format!("{} Allowed", g.check), skin.palette.style(Role::Ok)),
            ToolStatus::Pending => (
                format!("{} Pending", g.hourglass),
                skin.palette.style(Role::Warn),
            ),
            ToolStatus::Failed => (format!("{} Failed", g.cross), skin.palette.style(Role::Err)),
            ToolStatus::Denied => (format!("{} Denied", g.cross), skin.palette.style(Role::Err)),
        };
        let time = if call.time.is_empty() {
            g.dash.to_string()
        } else {
            call.time.clone()
        };
        let word_w = cols(&word);
        let mut line = lr(
            vec![
                Span::styled(
                    pad(&call.tool, TOOL_W, skin),
                    skin.palette.style(Role::Text),
                ),
                Span::styled(call.request.clone(), skin.palette.style(Role::Info)),
            ],
            vec![
                Span::styled(word, style),
                Span::styled(
                    format!(
                        "{}  {}",
                        " ".repeat(STATUS_W.saturating_sub(word_w)),
                        pad_left(&time, TIME_W, skin)
                    ),
                    dim,
                ),
            ],
            w,
            skin,
        );
        if r.selected_tool == Some(i) {
            line = banded(line, skin.palette.chip(Role::Accent));
        }
        set_row(content, i as u16 + 1, buf, line);
    }
    sink_footer(content, buf, r.tools.len() as u16 + 1, TOOLS_FOOTER, skin);
}

const TYPE_W: usize = 9;
const SIZE_W: usize = 7;
const MODIFIED_W: usize = 8;

fn render_artifacts(area: Rect, buf: &mut Buffer, r: &RunView, skin: &Skin) {
    if area.width < 12 || area.height < 3 {
        return;
    }
    let right = vec![Span::styled(
        format!("{} items", r.artifacts.len()),
        skin.palette.dim(),
    )];
    let content = card_frame(area, buf, skin, "ARTIFACTS / OUTPUT", right);
    let w = usize::from(content.width);
    let dim = skin.palette.dim();
    let header = lr(
        vec![Span::styled(
            format!("{}NAME", pad("TYPE", TYPE_W, skin)),
            dim,
        )],
        vec![Span::styled(
            format!(
                "{}  {}",
                pad_left("SIZE", SIZE_W, skin),
                pad_left("MODIFIED", MODIFIED_W, skin)
            ),
            dim,
        )],
        w,
        skin,
    );
    set_row(content, 0, buf, header);
    for (i, a) in r.artifacts.iter().enumerate() {
        let line = lr(
            vec![
                Span::styled(pad(&a.kind, TYPE_W, skin), skin.palette.style(Role::Accent)),
                Span::styled(a.name.clone(), skin.palette.style(Role::Text)),
            ],
            vec![
                Span::styled(
                    pad_left(&a.size, SIZE_W, skin),
                    skin.palette.style(Role::Text),
                ),
                Span::styled(
                    format!("  {}", pad_left(&a.modified, MODIFIED_W, skin)),
                    dim,
                ),
            ],
            w,
            skin,
        );
        set_row(content, i as u16 + 1, buf, line);
    }
    sink_footer(
        content,
        buf,
        r.artifacts.len() as u16 + 1,
        ARTIFACTS_FOOTER,
        skin,
    );
}

fn render_events(area: Rect, buf: &mut Buffer, r: &RunView, skin: &Skin, g: &PageGlyphs) {
    if area.width < 12 || area.height < 3 {
        return;
    }
    let right = vec![Span::styled(
        format!("View full {}", g.arrow),
        skin.palette.style(Role::Accent),
    )];
    let content = card_frame(area, buf, skin, "EVENT LOG", right);
    let w = usize::from(content.width);
    for (i, e) in r.events.iter().enumerate() {
        let (word, style) = match e.level {
            Level::Info => ("INFO", skin.palette.style(Role::Text)),
            Level::Warn => ("WARN", skin.palette.style(Role::Warn)),
            Level::Debug => ("DEBUG", skin.palette.dim()),
            Level::Error => ("ERROR", skin.palette.style(Role::Err)),
        };
        let text_style = match e.level {
            Level::Warn => skin.palette.style(Role::Warn),
            Level::Debug => skin.palette.dim(),
            Level::Error => skin.palette.style(Role::Err),
            Level::Info => skin.palette.style(Role::Text),
        };
        let line = lr(
            vec![
                Span::styled(format!("{}  ", e.time), skin.palette.dim()),
                Span::styled(pad(word, 6, skin), style),
                Span::styled(e.text.clone(), text_style),
            ],
            Vec::new(),
            w,
            skin,
        );
        set_row(content, i as u16, buf, line);
    }
}

fn render_bar(area: Rect, buf: &mut Buffer, skin: &Skin) {
    let block = Block::bordered()
        .border_set(skin.glyphs.border)
        .border_style(skin.palette.style(Role::Accent));
    let inner = block.inner(area);
    block.render(area, buf);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let mut left: Vec<Span<'static>> = vec![
        Span::styled("> ".to_string(), skin.palette.bold(Role::Accent)),
        Span::styled("Inspect commands:".to_string(), skin.palette.dim()),
    ];
    for (key, label) in BAR_COMMANDS {
        left.push(Span::raw("  "));
        left.push(Span::styled(
            format!("[{key}]"),
            skin.palette.style(Role::Accent),
        ));
        left.push(Span::styled(format!(" {label}"), skin.palette.dim()));
    }
    let right = vec![Span::styled(
        "[Enter] Select".to_string(),
        skin.palette.dim(),
    )];
    let line = lr(left, right, usize::from(inner.width), skin);
    buf.set_line(inner.x, inner.y, &line, inner.width);
}

// -- span carpentry ----------------------------------------------------------

/// Left spans, a gap, right spans, at exactly `w` columns. The right side
/// survives and the left truncates — the sidebar's trailing-column ruling.
fn lr(left: Vec<Span<'static>>, right: Vec<Span<'static>>, w: usize, skin: &Skin) -> Line<'static> {
    let rw: usize = right.iter().map(|s| cols(&s.content)).sum();
    let budget = w.saturating_sub(rw + usize::from(!right.is_empty()));
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

/// Left-only spans clipped to `w` columns, no right column.
fn clipped_line(spans: Vec<Span<'static>>, w: usize, skin: &Skin) -> Line<'static> {
    let mut out: Vec<Span<'static>> = Vec::new();
    let mut used = 0usize;
    for s in spans {
        let cw = cols(&s.content);
        if used + cw <= w {
            used += cw;
            out.push(s);
        } else {
            let room = w.saturating_sub(used);
            if room > 0 {
                out.push(Span::styled(clip(&s.content, room, skin), s.style));
            }
            break;
        }
    }
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

/// Paint one content row, clipped to the content area.
fn set_row(content: Rect, i: u16, buf: &mut Buffer, line: Line<'static>) {
    if i < content.height {
        buf.set_line(content.x, content.y + i, &line, content.width);
    }
}

/// [`fit`], made total for budgets narrower than the ellipsis — the memory
/// page's `clip`, duplicated because that one is private to its module.
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
    use super::super::palette::{Level as PaletteLevel, Palette};
    use super::super::render::UNICODE;
    use super::*;
    use ratatui::style::Color;

    fn skin() -> Skin {
        Skin::new(Palette::new(PaletteLevel::Truecolor), UNICODE)
    }

    fn step(name: &str, status: StepStatus, time: &str) -> Step {
        Step {
            name: name.into(),
            status,
            time: time.into(),
        }
    }

    /// The mock's sample data, in full. This is what the page looks like once
    /// a harness run exists to inspect; the fidelity is pinned now.
    fn populated() -> InspectView {
        use StepStatus::{Completed, Pending, Running};
        let steps = vec![
            step("Initialize run context", Completed, "1.2s"),
            step("Load workspace", Completed, "0.9s"),
            step("Gather context & memory", Completed, "2.8s"),
            step("Plan execution", Completed, "1.4s"),
            step("Verify alert details", Completed, "0.6s"),
            step("Check service health", Completed, "1.3s"),
            step("Review error rate", Completed, "1.1s"),
            step("Identify customer impact", Running, "2.6s"),
            step("Check dependencies", Pending, ""),
            step("Assess recent changes", Pending, ""),
            step("Mitigate or rollback", Pending, ""),
            step("Validate recovery", Pending, ""),
            step("Document findings", Pending, ""),
            step("Prepare summary", Pending, ""),
            step("Save artifacts", Pending, ""),
            step("Request approval", Pending, ""),
            step("Notify stakeholders", Pending, ""),
            step("Finalize run", Pending, ""),
        ];
        let tool = |t: &str, req: &str, status, time: &str| ToolCall {
            tool: t.into(),
            request: req.into(),
            status,
            time: time.into(),
        };
        let art = |kind: &str, name: &str, size: &str, modified: &str| ArtifactRow {
            kind: kind.into(),
            name: name.into(),
            size: size.into(),
            modified: modified.into(),
        };
        let ev = |time: &str, level, text: &str| Event {
            time: time.into(),
            level,
            text: text.into(),
        };
        InspectView {
            version: "v0.6.3".into(),
            run: Some(RunView {
                name: "checkout-triage".into(),
                id: "jd93f2a1".into(),
                status: "Running".into(),
                health: RunHealth::Ok,
                progress_pct: 70,
                started: "May 18 10:21:34".into(),
                started_ago: "18m 42s ago".into(),
                duration: "20m 13s".into(),
                eta: "eta 8m 32s".into(),
                model: "llama3:8b (local)".into(),
                context: "ctx 18,432 (68%)".into(),
                workspace: "~/workspace/my-project".into(),
                branch: "main @ 3f7c9d2".into(),
                agent: "orchestrator".into(),
                agent_runtime: "tokio".into(),
                steps,
                selected_step: 7,
                steps_done: 12,
                auto_approve: "Safe".into(),
                tools: vec![
                    tool(
                        "Shell",
                        "ps aux | grep checkout",
                        ToolStatus::Allowed,
                        "0.4s",
                    ),
                    tool(
                        "File Read",
                        "handlers/checkout.rs",
                        ToolStatus::Allowed,
                        "0.2s",
                    ),
                    tool(
                        "File Write",
                        "src/handlers/checkout.rs",
                        ToolStatus::Pending,
                        "1.8s",
                    ),
                    tool(
                        "Git",
                        "commit: update error handling",
                        ToolStatus::Pending,
                        "",
                    ),
                    tool(
                        "Memory",
                        "store impact assessment",
                        ToolStatus::Allowed,
                        "0.1s",
                    ),
                    tool(
                        "Web Fetch",
                        "GET https://status.payment-gateway.com",
                        ToolStatus::Allowed,
                        "0.6s",
                    ),
                ],
                selected_tool: Some(2),
                metadata: [
                    ("Run ID", "jd93f2a1"),
                    ("Parent Run", "8f3c2a1b7d4e9c12"),
                    ("Run Type", "Local (interactive)"),
                    ("Priority", "Normal"),
                    ("Retries", "0 / 3"),
                    ("Auto-approve", "Safe"),
                    ("Approval Policy", "Ask on write, network, git push"),
                    ("Model", "llama3:8b (local)"),
                    ("Context Size", "18,432 tokens (68%)"),
                    ("Prompt Tokens", "4,714"),
                    ("Completion Tokens", "8,128"),
                    ("Cache Hits", "87% (prompt) / 62% (completion)"),
                    ("Workspace", "~/workspace/my-project"),
                    ("Branch", "main @ 3f7c9d2"),
                    ("Worker Pool", "orchestrator, coder, reviewer, tester"),
                    ("Workers Active", "4 / 4"),
                    ("Agent", "orchestrator (tokio)"),
                    ("Initiated By", "you (interactive)"),
                ]
                .into_iter()
                .map(|(l, v)| (l.to_string(), v.to_string()))
                .collect(),
                artifacts: vec![
                    art(
                        "Patch",
                        "fix/checkout-error-handling.patch",
                        "6.3 KB",
                        "10:38:52",
                    ),
                    art("File", "src/handlers/checkout.rs", "4.1 KB", "10:38:49"),
                    art("Summary", "impact-assessment.md", "2.2 KB", "10:38:50"),
                    art("Trace", "trace.json", "128 KB", "10:38:53"),
                    art("Log", "run.log", "18 KB", "10:38:53"),
                    art("Report", "run-summary.html", "12 KB", "10:38:53"),
                ],
                events: vec![
                    // The WARN and DEBUG texts are the mock's; the INFO rows
                    // are authored sample data (design note).
                    ev(
                        "10:38:41",
                        Level::Info,
                        "Step 08 started: Identify customer impact",
                    ),
                    ev(
                        "10:38:44",
                        Level::Info,
                        "Tool allowed: Shell ps aux | grep checkout",
                    ),
                    ev(
                        "10:38:49",
                        Level::Warn,
                        "File write requested: checkout.rs (pending)",
                    ),
                    ev(
                        "10:38:51",
                        Level::Debug,
                        "Worker coder streaming response...",
                    ),
                    ev("10:38:53", Level::Info, "Artifact saved: trace.json"),
                ],
            }),
        }
    }

    /// A tall terminal is filled: the TOOL CALLS | ARTIFACTS pair absorbs the
    /// height the event log and the bar leave over, so the event log sits low
    /// on the screen rather than where the pair's row count alone would put
    /// it. The pin is relative: the same view at +20 rows must push EVENT LOG
    /// at least 15 rows further down, which only happens if the pair grew.
    #[test]
    fn the_tools_pair_absorbs_a_taller_screen() {
        let v = populated();
        let (_, short) = locate(&draw(&v, 161, 75), "EVENT LOG");
        let (_, tall) = locate(&draw(&v, 161, 95), "EVENT LOG");
        assert!(
            tall >= short + 15,
            "EVENT LOG moved {short} -> {tall}; the pair did not flex"
        );
    }

    /// The bar is reserved from the bottom, so it survives a screen too short
    /// for every card. A page that drops its own input control when the
    /// terminal shrinks is the defect this pins.
    #[test]
    fn the_input_bar_holds_the_bottom_on_a_short_screen() {
        let rows = draw(&populated(), 161, 30);
        let (_, y) = locate(&rows, "Inspect commands:");
        assert!(
            y >= 27,
            "the bar drifted up to row {y} instead of the floor"
        );
    }

    /// Today's truth: no harness run exists to inspect.
    fn empty() -> InspectView {
        InspectView {
            version: "v0.6.3".into(),
            run: None,
        }
    }

    fn buffer(v: &InspectView, w: u16, h: u16) -> Buffer {
        let area = Rect::new(0, 0, w, h);
        let mut buf = Buffer::empty(area);
        render(area, &mut buf, v, &skin());
        buf
    }

    fn lines(buf: &Buffer) -> Vec<String> {
        // A double-width glyph (the pending tool's hourglass) owns two cells;
        // the continuation cell reads back as a space and would double-count
        // the column, so it is skipped.
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

    fn draw(v: &InspectView, w: u16, h: u16) -> Vec<String> {
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

    // -- the head ------------------------------------------------------------

    #[test]
    fn the_head_is_version_title_subtitle_rule() {
        let rows = draw(&populated(), 161, 75);
        assert!(
            rows[0].ends_with("v0.6.3"),
            "version not in the corner: {:?}",
            rows[0]
        );
        assert_eq!(rows[1], "Inspect Run");
        assert_eq!(rows[2], SUBTITLE);
        assert!(
            rows[3].chars().all(|c| c == '─') && !rows[3].is_empty(),
            "rule missing"
        );
    }

    // -- the header info strip ----------------------------------------------

    #[test]
    fn the_strip_is_seven_labeled_cells_in_one_band() {
        let rows = draw(&populated(), 161, 75);
        let (_, y0) = locate(&rows, "RUN");
        for label in STRIP_LABELS {
            let (_, y) = locate(&rows, label);
            assert_eq!(y, y0, "{label} not in the strip band");
        }
        // The mock's DUARTION is a typo; the rendered label is DURATION.
        assert!(
            !rows.join("\n").contains("DUARTION"),
            "the mock's typo leaked"
        );
        // Seven bordered cells: the label row's line above holds eight+ top
        // corners across the width (adjacent cells).
        let tops = rows[usize::from(y0) - 1].matches('╭').count();
        assert_eq!(tops, 7, "not seven cells: {tops} tops");
    }

    #[test]
    fn the_strip_cells_carry_the_mocks_values() {
        let rows = draw(&populated(), 161, 75);
        let all = rows.join("\n");
        for value in [
            "checkout-triage",
            "jd93f2a1",
            "● Running",
            "May 18 10:21:34",
            "18m 42s ago",
            "20m 13s",
            "eta 8m 32s",
            "llama3:8b (local)",
            "ctx 18,432 (68%)",
            "~/workspace/my-project",
            "⎇ main @ 3f7c9d2",
            "orchestrator",
            "tokio",
        ] {
            assert!(all.contains(value), "{value:?} missing from the strip");
        }
        let gauge = row_with(&rows, "70%");
        assert!(
            gauge.contains('█') && gauge.contains('░'),
            "mini-gauge missing: {gauge:?}"
        );
        // The status dot is green (Role::Ok), not hardcoded chrome.
        let buf = buffer(&populated(), 161, 75);
        let (sx, sy) = locate(&rows, "● Running");
        assert_eq!(
            buf[(sx, sy)].style().fg,
            Some(skin().palette.color(Role::Ok)),
            "status dot not Ok-colored"
        );
    }

    // -- the step timeline ---------------------------------------------------

    #[test]
    fn the_step_timeline_is_the_mocks_18_rows_in_order() {
        let rows = draw(&populated(), 161, 75);
        let header = row_with(&rows, "STEP TIMELINE / EXECUTION BREAKDOWN");
        assert!(
            header.contains("12 / 18 steps   Expand: [e]"),
            "step count/affordance missing: {header:?}"
        );
        let done = row_with(&rows, "Initialize run context");
        assert!(done.contains("01"), "step number missing: {done:?}");
        assert!(
            done.contains("✓ Completed"),
            "completed dress missing: {done:?}"
        );
        assert!(done.contains("1.2s"), "time missing: {done:?}");
        let running = row_with(&rows, "Identify customer impact");
        assert!(
            running.contains("↻ Running"),
            "running dress missing: {running:?}"
        );
        assert!(
            running.contains("2.6s"),
            "running time missing: {running:?}"
        );
        let pending = row_with(&rows, "Check dependencies");
        assert!(
            pending.contains("● Pending"),
            "pending dress missing: {pending:?}"
        );
        assert!(pending.contains('–'), "pending dash missing: {pending:?}");
        // Order: each step's row is below the previous step's.
        let mut last = 0u16;
        for name in [
            "Initialize run context",
            "Plan execution",
            "Identify customer impact",
            "Finalize run",
        ] {
            let (_, y) = locate(&rows, name);
            assert!(y > last, "{name} out of order");
            last = y;
        }
    }

    #[test]
    fn the_selected_step_wears_the_accent_border_and_the_prefix() {
        let buf = buffer(&populated(), 161, 75);
        let rows = lines(&buf);
        let row = row_with(&rows, "Identify customer impact");
        assert!(row.contains("▷ 08"), "selection prefix missing: {row:?}");
        let (x, y) = locate(&rows, "Identify customer impact");
        // The row above and below carry the selection border, in the accent.
        for dy in [y - 1, y + 1] {
            assert_eq!(
                buf[(x, dy)].symbol(),
                "─",
                "no border row beside the selection"
            );
            assert_eq!(
                buf[(x, dy)].style().fg,
                Some(accent()),
                "selection border not accent"
            );
        }
        // An unselected row has no such border under it.
        let (x0, y0) = locate(&rows, "Check dependencies");
        assert_ne!(
            buf[(x0, y0 + 1)].symbol(),
            "─",
            "border leaked to an unselected row"
        );
    }

    #[test]
    fn the_selection_follows_the_view() {
        let mut v = populated();
        v.run.as_mut().unwrap().selected_step = 2;
        let rows = draw(&v, 161, 75);
        let row = row_with(&rows, "Gather context & memory");
        assert!(row.contains("▷ 03"), "selection did not move: {row:?}");
        assert!(
            !rows.join("\n").contains("▷ 08"),
            "old selection still dressed"
        );
    }

    // -- run metadata --------------------------------------------------------

    #[test]
    fn run_metadata_rows_carry_the_mocks_pairs_flush_right() {
        let rows = draw(&populated(), 161, 75);
        row_with(&rows, "RUN METADATA");
        for (label, value) in [
            ("Parent Run", "8f3c2a1b7d4e9c12"),
            ("Approval Policy", "Ask on write, network, git push"),
            ("Cache Hits", "87% (prompt) / 62% (completion)"),
            ("Worker Pool", "orchestrator, coder, reviewer, tester"),
            ("Initiated By", "you (interactive)"),
        ] {
            let row = row_with(&rows, label);
            assert!(
                row.contains(value),
                "{value:?} missing beside {label:?}: {row:?}"
            );
            flush_right_of(row, value);
        }
    }

    // -- tool calls ----------------------------------------------------------

    #[test]
    fn the_tool_table_has_the_mocks_columns_rows_and_header() {
        let rows = draw(&populated(), 161, 75);
        let header = row_with(&rows, "TOOL CALLS / APPROVALS");
        assert!(
            header.contains("Auto-approve: Safe"),
            "auto-approve missing: {header:?}"
        );
        let cols_row = row_with(&rows, "REQUEST");
        for c in ["TOOL", "STATUS", "TIME"] {
            assert!(cols_row.contains(c), "{c} column missing: {cols_row:?}");
        }
        let shell = row_with(&rows, "ps aux | grep checkout");
        assert!(
            shell.contains("✓ Allowed"),
            "allowed dress missing: {shell:?}"
        );
        assert!(shell.contains("0.4s"), "time missing: {shell:?}");
        // "src/handlers/checkout.rs" also names an artifact whose card shares
        // physical rows with this one; find the write row by its TOOL column.
        let write = row_with(&rows, "File Write");
        assert!(
            write.contains("⏳ Pending"),
            "pending dress missing: {write:?}"
        );
        assert!(write.contains("1.8s"), "pending time missing: {write:?}");
        let git = row_with(&rows, "commit: update error handling");
        assert!(
            git.contains("⏳ Pending"),
            "git pending dress missing: {git:?}"
        );
        assert!(git.contains('–'), "git dash missing: {git:?}");
        row_with(&rows, "store impact assessment");
        row_with(&rows, "GET https://status.payment-gateway.com");
    }

    #[test]
    fn the_highlighted_tool_row_wears_the_band() {
        let buf = buffer(&populated(), 161, 75);
        let rows = lines(&buf);
        let (x, y) = locate(&rows, "File Write");
        assert_eq!(
            buf[(x, y)].style().bg,
            Some(accent()),
            "no band on the highlighted row"
        );
        let (x0, y0) = locate(&rows, "File Read");
        assert_ne!(
            buf[(x0, y0)].style().bg,
            Some(accent()),
            "band leaked to another row"
        );
    }

    // -- artifacts -----------------------------------------------------------

    #[test]
    fn the_artifact_table_has_the_mocks_columns_and_rows() {
        let rows = draw(&populated(), 161, 75);
        let header = row_with(&rows, "ARTIFACTS / OUTPUT");
        assert!(header.contains("6 items"), "item count missing: {header:?}");
        let cols_row = row_with(&rows, "MODIFIED");
        for c in ["TYPE", "NAME", "SIZE"] {
            assert!(cols_row.contains(c), "{c} column missing: {cols_row:?}");
        }
        for (name, size, modified) in [
            ("fix/checkout-error-handling.patch", "6.3 KB", "10:38:52"),
            ("impact-assessment.md", "2.2 KB", "10:38:50"),
            ("trace.json", "128 KB", "10:38:53"),
            ("run-summary.html", "12 KB", "10:38:53"),
        ] {
            let row = row_with(&rows, name);
            assert!(row.contains(size), "{size} missing beside {name}: {row:?}");
            assert!(
                row.contains(modified),
                "{modified} missing beside {name}: {row:?}"
            );
        }
        row_with(&rows, "Patch");
        row_with(&rows, "Summary");
    }

    // -- event log -----------------------------------------------------------

    #[test]
    fn the_event_log_levels_wear_their_colors() {
        let buf = buffer(&populated(), 161, 75);
        let rows = lines(&buf);
        let header = row_with(&rows, "EVENT LOG");
        assert!(
            header.contains("View full →"),
            "view-full affordance missing: {header:?}"
        );
        let (ax, ay) = locate(&rows, "View full →");
        assert_eq!(
            buf[(ax, ay)].style().fg,
            Some(accent()),
            "affordance not accent"
        );
        let (wx, wy) = locate(&rows, "File write requested: checkout.rs (pending)");
        assert_eq!(
            buf[(wx, wy)].style().fg,
            Some(skin().palette.color(Role::Warn)),
            "WARN row not amber"
        );
        row_with(&rows, "Worker coder streaming response...");
        let warn_row = row_with(&rows, "File write requested");
        assert!(
            warn_row.contains("WARN"),
            "level word missing: {warn_row:?}"
        );
        let dbg_row = row_with(&rows, "Worker coder streaming");
        assert!(dbg_row.contains("DEBUG"), "level word missing: {dbg_row:?}");
        assert!(
            dbg_row.contains("10:38:51"),
            "timestamp missing: {dbg_row:?}"
        );
    }

    // -- footers and the input bar, byte for byte ----------------------------

    #[test]
    fn the_three_card_footers_are_the_mocks_exactly() {
        let rows = draw(&populated(), 161, 75);
        row_with(&rows, STEPS_FOOTER);
        row_with(&rows, TOOLS_FOOTER);
        row_with(&rows, ARTIFACTS_FOOTER);
        assert_eq!(
            STEPS_FOOTER,
            "[↑/↓] Select  [Enter] Focus  [e] Expand  [c] Collapse All"
        );
        assert_eq!(
            TOOLS_FOOTER,
            "[t] Filter  [Enter] Inspect  [a] Approve  [d] Deny  [o] Open"
        );
        assert_eq!(
            ARTIFACTS_FOOTER,
            "[o] Open  [v] View  [d] Diff  [y] Copy Path"
        );
    }

    #[test]
    fn the_input_bar_is_the_mocks() {
        let rows = draw(&populated(), 161, 75);
        let bar = row_with(&rows, "Inspect commands:");
        let inner = bar.trim_start_matches(['│', ' ']);
        assert!(inner.starts_with("> "), "prompt prefix missing: {bar:?}");
        assert!(
            bar.contains(
                "Inspect commands:  [e] expand step  [t] open trace  [d] view diff  \
                 [x] export run  [b] back to runs"
            ),
            "commands not verbatim: {bar:?}"
        );
        assert!(
            bar.contains("[Enter] Select"),
            "select hint missing: {bar:?}"
        );
    }

    // -- the honest empty page ----------------------------------------------

    #[test]
    fn the_empty_page_is_the_head_and_one_honest_line() {
        let rows = draw(&empty(), 161, 75);
        assert_eq!(rows[1], "Inspect Run");
        row_with(&rows, EMPTY_PAGE);
        let after_head = rows[4..].iter().filter(|r| !r.is_empty()).count();
        assert_eq!(
            after_head, 1,
            "the empty page shows more than the honest line"
        );
    }

    #[test]
    fn the_empty_page_makes_no_false_claims() {
        let all = draw(&empty(), 161, 75).join("\n");
        for sample in [
            "checkout-triage",
            "jd93f2a1",
            "Running",
            "70%",
            "llama3:8b",
            "STEP TIMELINE",
            "RUN METADATA",
            "TOOL CALLS",
            "ARTIFACTS",
            "EVENT LOG",
            "Inspect commands:",
            "Auto-approve",
        ] {
            assert!(
                !all.contains(sample),
                "sample or chrome leaked when no run exists: {sample}"
            );
        }
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
            &Skin::new(Palette::new(PaletteLevel::Ansi16), ASCII),
        );
        for y in 0..60u16 {
            let row: String = (0..120u16)
                .map(|x| buf[(x, y)].symbol().to_string())
                .collect();
            assert!(row.is_ascii(), "non-ASCII under the ASCII skin: {row:?}");
        }
    }

    /// Eyeball dump: `cargo test -p emma the_populated_inspect -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn the_populated_inspect_page_at_161x75_for_eyeballing() {
        for row in draw(&populated(), 161, 75) {
            println!("{row}");
        }
    }
}
