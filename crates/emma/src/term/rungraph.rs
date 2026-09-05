//! The Run Graph page: the owner's mock, cell for cell.
//!
//! The third harness view (harness plan H0, design in
//! the Run Graph page design): a box-drawn execution DAG on the left,
//! the SELECTED NODE inspector on the right, a five-panel strip beneath, and
//! the page command bar, with every label, glyph and affordance from the
//! mock.
//!
//! The one sanctioned divergence is data, the Memory page's rule. The
//! harness the mock describes is aspirational; a live [`GraphView`] with no
//! run keeps the exact chrome, says "No run graph — a run must be active" in
//! the canvas, and shows real zeros and dashes everywhere a value would be a
//! claim. The legend still renders in full because it explains vocabulary
//! rather than claiming state. Tests pin both faces.
//!
//! The layout is owned here: layered top-down (a node's layer is one past
//! its deepest parent), uniform boxes, siblings spread and centered per
//! layer, three gap rows between layers that belong to the dashed edges and
//! their arrowheads. No force direction. The canvas is centered
//! horizontally and never wider than its panel, so the only offset that can
//! exist is vertical: [`GraphView::scroll`] is the row the canvas starts at,
//! the renderer blits a window of the laid-out graph at that offset, and the
//! footer counts what is hidden above and below. Keys stay integration's.
//!
//! Pure rendering, like [`super::settings`] and [`super::memory`]: the shell
//! owns whether the page is open, keys and mounting are integration's. All
//! width arithmetic is in display columns via [`cols`]/[`fit`], and no row
//! ever writes past its panel's inner width.

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

/// What a node is, as far as its glyph and border are concerned. The mock's
/// output node wears a green border whatever its status; that is a property
/// of being the sink, so it lives here rather than in sample data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// An agent worker: orchestrator, planner, coder, reviewer, tester.
    Agent,
    /// A tool executor: shell, file write, git.
    Tool,
    /// A produced artifact set.
    Artifact,
    /// The memory store.
    Memory,
    /// The run's terminal output/artifacts sink.
    Output,
}

/// A node's lifecycle state. Decides the status dot, the word beside it, and
/// (with [`Kind`] and selection) the box border.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Running,
    Completed,
    Pending,
    Queued,
    Failed,
    Skipped,
}

impl Status {
    pub fn word(self) -> &'static str {
        match self {
            Status::Running => "Running",
            Status::Completed => "Completed",
            Status::Pending => "Pending",
            Status::Queued => "Queued",
            Status::Failed => "Failed",
            Status::Skipped => "Skipped",
        }
    }

    /// The status dot's colour: the legend's vocabulary.
    fn dot_role(self) -> Role {
        match self {
            Status::Running | Status::Completed => Role::Ok,
            Status::Pending => Role::Warn,
            Status::Queued => Role::Info,
            Status::Failed => Role::Err,
            Status::Skipped => Role::Dim,
        }
    }

    /// The box border's colour when the node is neither selected nor the
    /// output sink: the mock paints running and pending amber, queued
    /// violet, failed red, and the finished dim.
    fn border_role(self) -> Role {
        match self {
            Status::Running | Status::Pending => Role::Warn,
            Status::Queued => Role::Info,
            Status::Failed => Role::Err,
            Status::Completed | Status::Skipped => Role::Dim,
        }
    }
}

/// One node of the DAG. Parents are indices into [`GraphView::nodes`]; an
/// index at or past the node's own position is ignored rather than trusted
/// (the layout must terminate on any input).
#[derive(Debug, Clone)]
pub struct Node {
    /// The short id the mock prints as `id: nX`.
    pub id: String,
    pub name: String,
    pub kind: Kind,
    pub status: Status,
    /// Display time (`4.2s`), or `–` before the node has run.
    pub time: String,
    pub parents: Vec<usize>,
}

/// An event level in the RECENT EVENTS section.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventLevel {
    Info,
    Debug,
    Trace,
}

impl EventLevel {
    fn label(self) -> &'static str {
        match self {
            EventLevel::Info => "INFO",
            EventLevel::Debug => "DEBUG",
            EventLevel::Trace => "TRACE",
        }
    }
}

/// One RECENT EVENTS row.
#[derive(Debug, Clone)]
pub struct Event {
    pub time: String,
    pub level: EventLevel,
    pub text: String,
}

/// What the SELECTED NODE panel knows beyond the DAG: runtime detail the
/// graph does not carry. Parent and children rows derive from the DAG and
/// are not repeated here.
#[derive(Debug, Clone, Default)]
pub struct NodeDetail {
    /// The Type row's value (`orchestrator`).
    pub node_type: String,
    pub started: String,
    pub latency: String,
    /// `0 / 3` shapes, caller-formatted.
    pub retries: String,
    pub worker: String,
    pub host: String,
    pub pid: String,
    /// INPUTS rows, label then value.
    pub inputs: Vec<(String, String)>,
    /// OUTPUTS rows, label then value.
    pub outputs: Vec<(String, String)>,
    pub events: Vec<Event>,
}

/// The RUN SUMMARY panel's non-derivable half. Progress and its gauge are
/// derived from the DAG instead, so the strip can never disagree with the
/// canvas.
#[derive(Debug, Clone)]
pub struct RunSummary {
    pub run_id: String,
    pub status: Status,
    pub started: String,
    pub elapsed: String,
    pub eta: String,
    /// Work done of work planned, when something recorded a denominator.
    ///
    /// `None` falls back to the DAG's own completed-of-total, which is the
    /// only fraction a graph with no task list can offer. A run that ticked a
    /// task file has a better one, and it is about the work rather than about
    /// the machinery, so it wins when it exists.
    pub progress: Option<(u64, u64)>,
}

/// The GRAPH METRICS rows that are not derivable from the DAG: path shape
/// and worker facts the harness will supply. The count rows (total,
/// completed, running, pending/queued, failed) derive from the nodes.
#[derive(Debug, Clone)]
pub struct Metrics {
    pub critical_path: u64,
    pub parallel_branches: u64,
    pub longest_task: String,
    pub avg_task_time: String,
    pub active_workers: String,
}

/// The FILTERS panel's four rows. `All`/`–` is the untouched state and is
/// chrome, not a claim.
#[derive(Debug, Clone)]
pub struct Filters {
    pub status: String,
    pub kind: String,
    pub workers: String,
    pub search: String,
}

impl Default for Filters {
    fn default() -> Self {
        Filters {
            status: "All".into(),
            kind: "All".into(),
            workers: "All".into(),
            search: "–".into(),
        }
    }
}

/// What the page shows. Empty `nodes` is the live truth today: no harness,
/// no run, honest chrome.
#[derive(Debug, Clone, Default)]
pub struct GraphView {
    /// The running binary's version, `v`-prefixed — the mock's top-right corner.
    pub version: String,
    pub nodes: Vec<Node>,
    /// The selected node, if any: accent border, and it feeds the side panel.
    pub selected: Option<usize>,
    /// Runtime detail for the selected node, when the caller has only one.
    pub detail: Option<NodeDetail>,
    /// Runtime detail per node, parallel to [`Self::nodes`]. A caller that
    /// knows every node fills this and the panel follows the selection;
    /// an empty vec falls back to [`Self::detail`], so the mock's single-node
    /// shape still renders.
    pub details: Vec<NodeDetail>,
    pub summary: Option<RunSummary>,
    /// Depths in [`QUEUE_LABELS`] order.
    pub queue_depths: [u64; 5],
    pub metrics: Option<Metrics>,
    pub filters: Filters,
    /// The command bar's current text; empty shows the placeholder.
    pub query: String,
    /// Whether the command bar has the keyboard. The page's own keys (`b`,
    /// `?`, the arrows) are letters too, so typing has to be a mode rather
    /// than a default, or `b` could never be typed into a command.
    pub palette: bool,
    /// One line above the command bar, until a key clears it. The honesty
    /// channel: a command whose backend does not exist says so here, the
    /// dashboard's `notice` rule applied to this page.
    pub notice: Option<String>,
    /// The first graph row the DAG canvas shows. Vertical only: a layer is
    /// centered and shrunk to the panel's width, so nothing is ever off to
    /// the side and a horizontal pan would move nothing.
    pub scroll: u16,
    /// How many rows the canvas had on the last paint, recorded by
    /// [`render`] and handed back through the harness (the `render_hits`
    /// rule). A pure key handler cannot measure a viewport it never sees,
    /// and clamping without it would guess.
    pub canvas_rows: u16,
}

/// How many rows the laid-out graph needs: one box per layer, the gap rows
/// between them, no trailing gap. Pure, because a node's row depends only on
/// its layer, which is why scroll-to-selection needs no paint.
pub fn content_rows(nodes: &[Node]) -> u16 {
    let n = layers(nodes).iter().max().map_or(0, |&m| m + 1);
    if n == 0 {
        0
    } else {
        n * BOX_H + (n - 1) * VGAP
    }
}

/// The largest honest scroll: the last row of the graph sits on the last row
/// of the canvas, never past it. Zero when everything fits.
pub fn max_scroll(v: &GraphView) -> u16 {
    content_rows(&v.nodes).saturating_sub(v.canvas_rows)
}

/// Move the viewport by `rows`, clamped at both ends.
pub fn pan(v: &mut GraphView, down: bool, rows: u16) {
    let max = max_scroll(v);
    v.scroll = if down {
        v.scroll.saturating_add(rows).min(max)
    } else {
        v.scroll.saturating_sub(rows)
    };
}

/// Jump the viewport to the first or the last row.
pub fn pan_to(v: &mut GraphView, top: bool) {
    v.scroll = if top { 0 } else { max_scroll(v) };
}

/// Bring the selected node fully into view, moving the viewport the least
/// distance that does it. This is what makes navigation behave like every
/// other cursor: the selection leads and the canvas follows.
pub fn reveal_selected(v: &mut GraphView) {
    let (Some(i), rows) = (v.selected, v.canvas_rows) else {
        return;
    };
    if rows == 0 || i >= v.nodes.len() {
        return;
    }
    let top = layers(&v.nodes)[i] * (BOX_H + VGAP);
    let bottom = top + BOX_H;
    if top < v.scroll {
        v.scroll = top;
    } else if bottom > v.scroll + rows {
        v.scroll = bottom - rows;
    }
    v.scroll = v.scroll.min(max_scroll(v));
}

/// The subtitle under the title, verbatim from the mock.
pub const SUBTITLE: &str = "Execution DAG, worker flow, and tool relationships";

/// The QUEUE DEPTH rows, closed set, in the mock's order.
pub const QUEUE_LABELS: [&str; 5] = [
    "Global Queue",
    "Worker Queue",
    "Shell Queue",
    "Test Queue",
    "Memory Queue",
];

/// The honest empty states, one dim line inside the exact chrome.
pub const EMPTY_GRAPH: &str = "No run graph — a run must be active";
pub const EMPTY_NODE: &str = "No node selected — a run must be active";

/// The command bar's placeholder, verbatim from the mock.
pub const PLACEHOLDER: &str = "Command palette (run, inspect, metrics, clear, ...)";

/// The bar's right-hand hint, verbatim from the mock.
pub const HINT_EXECUTE: &str = "[Enter] Execute";

/// What the palette can actually do, named in full so an unknown command can
/// answer with the truth rather than with silence.
pub const COMMANDS: &str = "commands: node <n>, inspect, metrics, clear, help";

/// The honest answers. Every key the mock's placeholder promises exists here;
/// the ones with no backend say which part is missing rather than going dead.
pub const NOTICE_RUN: &str =
    "the Run Graph is read-only: it draws session history, not a process manager";
pub const NOTICE_METRICS: &str =
    "GRAPH METRICS is on screen; queue depth, workers and ETA have no backing record";
pub const NOTICE_NO_NODE: &str = "no node to inspect: this run graph is empty";
pub const NOTICE_CLEARED: &str = "filters reset and selection returned to the root";

/// One typed command, against the mounted view. Pure: it reads and rewrites
/// the view and touches nothing else, so the shell can call it from a key
/// handler that is not allowed to touch disk.
///
/// The query is consumed either way, because a bar that keeps the text after
/// running it cannot tell you whether it ran.
pub fn command(v: &mut GraphView) {
    let text = std::mem::take(&mut v.query);
    let text = text.trim();
    let (head, rest) = match text.split_once(char::is_whitespace) {
        Some((h, r)) => (h, r.trim()),
        None => (text, ""),
    };
    v.notice = match head {
        "" => None,
        "clear" => {
            v.filters = Filters::default();
            v.selected = (!v.nodes.is_empty()).then_some(0);
            Some(NOTICE_CLEARED.to_string())
        }
        "inspect" => match select_node(v, rest) {
            Some(i) => Some(format!(
                "{}: {} {}",
                v.nodes[i].id,
                v.nodes[i].name,
                v.nodes[i].status.word()
            )),
            None if v.nodes.is_empty() => Some(NOTICE_NO_NODE.to_string()),
            None => Some(COMMANDS.to_string()),
        },
        "node" => match select_node(v, rest) {
            Some(i) => Some(format!("selected {}", v.nodes[i].id)),
            None if v.nodes.is_empty() => Some(NOTICE_NO_NODE.to_string()),
            None => Some(format!("no such node: {rest:?}")),
        },
        "metrics" => Some(NOTICE_METRICS.to_string()),
        "run" => Some(NOTICE_RUN.to_string()),
        "help" => Some(COMMANDS.to_string()),
        other => Some(format!("no such command: {other:?}. {COMMANDS}")),
    };
}

/// The node a command names: `n3`, `3`, or the selected one when unnamed.
fn select_node(v: &mut GraphView, arg: &str) -> Option<usize> {
    if v.nodes.is_empty() {
        return None;
    }
    if arg.is_empty() {
        let i = v.selected.filter(|&i| i < v.nodes.len()).unwrap_or(0);
        v.selected = Some(i);
        return Some(i);
    }
    let i = arg
        .strip_prefix('n')
        .unwrap_or(arg)
        .parse::<usize>()
        .ok()
        .filter(|&i| i < v.nodes.len())?;
    v.selected = Some(i);
    Some(i)
}

/// Move the selection by one node, wrapping. The DAG footer promises this
/// key and the selection is the one piece of graph state a key can change.
pub fn navigate(v: &mut GraphView, forward: bool) {
    let n = v.nodes.len();
    if n == 0 {
        return;
    }
    let cur = v.selected.unwrap_or(0).min(n - 1);
    v.selected = Some(if forward {
        (cur + 1) % n
    } else {
        (cur + n - 1) % n
    });
    reveal_selected(v);
}

// endregion: State

// region: Rendering
// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

/// A node box: three content rows inside a border.
const BOX_H: u16 = 5;
/// Air between siblings in a layer.
const HGAP: u16 = 2;
/// Gap rows between layers; the edges live here (stub, run, arrowhead).
const VGAP: u16 = 3;

/// The page's own glyphs, one ASCII fallback each — the `render::ASCII`
/// split, extended for shapes that file does not carry.
struct PageGlyphs {
    agent: &'static str,
    tool: &'static str,
    artifact: &'static str,
    memory: &'static str,
    output: &'static str,
    dot: &'static str,
    check: &'static str,
    cross: &'static str,
    skip: &'static str,
    kick: &'static str,
    vdash: &'static str,
    hdash: &'static str,
    arrow: &'static str,
    tri_down: &'static str,
    tri_up: &'static str,
    full: &'static str,
    empty: &'static str,
    /// The DAG footer's navigate keys.
    up_down: &'static str,
}

fn page_glyphs(skin: &Skin) -> PageGlyphs {
    if skin.glyphs == ASCII {
        PageGlyphs {
            agent: "*",
            tool: "%",
            artifact: "=",
            memory: "o",
            output: "#",
            dot: "*",
            check: "+",
            cross: "x",
            skip: "-",
            kick: "~",
            vdash: "|",
            hdash: "-",
            arrow: "v",
            tri_down: "v",
            tri_up: "^",
            full: "#",
            empty: "-",
            up_down: "^/v",
        }
    } else {
        PageGlyphs {
            agent: "◆",
            tool: "⚙",
            artifact: "▤",
            memory: "◇",
            output: "▣",
            dot: "●",
            check: "✓",
            cross: "✗",
            skip: "─",
            kick: "↻",
            vdash: "╎",
            hdash: "╌",
            arrow: "▼",
            tri_down: "▾",
            tri_up: "▴",
            full: "█",
            empty: "░",
            up_down: "↑/↓",
        }
    }
}

impl Kind {
    fn glyph(self, g: &PageGlyphs) -> &'static str {
        match self {
            Kind::Agent => g.agent,
            Kind::Tool => g.tool,
            Kind::Artifact => g.artifact,
            Kind::Memory => g.memory,
            Kind::Output => g.output,
        }
    }
}

impl Status {
    fn glyph(self, g: &PageGlyphs) -> &'static str {
        match self {
            Status::Running | Status::Pending | Status::Queued => g.dot,
            Status::Completed => g.check,
            Status::Failed => g.cross,
            Status::Skipped => g.skip,
        }
    }
}

/// The honest strings and sample dashes carry `—`/`–`; a legacy code page
/// gets hyphens instead.
fn dashes(text: &str, ascii: bool) -> String {
    if ascii {
        text.replace(['—', '–'], "-")
    } else {
        text.to_string()
    }
}

/// The five-panel strip's fixed height: GRAPH METRICS is the tallest at ten
/// value rows plus its affordance, inside a header and a border.
const STRIP_H: u16 = 14;

/// The fewest rows the DAG canvas is ever given: a border, a header, one
/// whole node box and the footer. Below that the panel would draw nothing and
/// the page's own head rows would be left hanging over the strip with no box
/// under them, which is what a short terminal used to show.
const MIN_MAIN: u16 = BOX_H + 4;

/// Draw the whole page into `area`. Too small an area draws what fits from
/// the top, clipped whole-row like every other widget.
///
/// Returns how many rows the DAG canvas got, which is the one fact about the
/// viewport that only the paint knows. The shell stores it back on the view
/// so the pure key handler can clamp a scroll against it.
pub fn render(area: Rect, buf: &mut Buffer, v: &GraphView, skin: &Skin) -> u16 {
    if area.width < 4 || area.height < 2 {
        return 0;
    }
    let w = usize::from(area.width);
    let g = page_glyphs(skin);
    let bottom = area.y + area.height;
    let mut y = area.y;

    // The head: version in the corner, the title, the subtitle, a rule —
    // one bold accent row for "large", the settings design Q8 ruling.
    let version = fit(&v.version, w, skin.glyphs.ellipsis);
    let mut canvas_rows = 0u16;
    let head = [
        Line::from(vec![
            Span::raw(" ".repeat(w.saturating_sub(cols(&version)))),
            Span::styled(version, skin.palette.dim()),
        ]),
        Line::from(Span::styled(
            fit("Run Graph", w, skin.glyphs.ellipsis),
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
            return canvas_rows;
        }
        buf.set_line(area.x, y, &line, area.width);
        y += 1;
    }

    // The action bar: bracketed accent key, dim label, the mock's two
    // columns of air, and Follow's "(Live)" in green.
    if y >= bottom {
        return canvas_rows;
    }
    let actions: [(&str, String); 7] = [
        ("z", format!("Zoom {}", g.tri_down)),
        ("Z", format!("Zoom {}", g.tri_up)),
        ("f", "Filter".into()),
        ("t", "Trace".into()),
        ("c", "Center".into()),
        ("x", "Collapse".into()),
        ("F", "Follow".into()),
    ];
    let mut bar: Vec<Span<'static>> = Vec::new();
    for (i, (key, label)) in actions.into_iter().enumerate() {
        if i > 0 {
            bar.push(Span::raw("  "));
        }
        bar.push(Span::styled(
            format!("[{key}]"),
            skin.palette.style(Role::Accent),
        ));
        bar.push(Span::styled(format!(" {label}"), skin.palette.dim()));
    }
    bar.push(Span::styled(
        " (Live)".to_string(),
        skin.palette.style(Role::Ok),
    ));
    buf.set_line(area.x, y, &Line::from(clip_spans(bar, w, skin)), area.width);
    y += 2; // the bar, then one row of air before the main split

    // The main split above the strip above the command bar.
    //
    // The heights are decided here rather than handed to a solver, because a
    // solver that honours `Length(14)` on a short terminal leaves the canvas
    // zero rows and the page becomes a title floating over a strip, with the
    // graph and its border gone. The canvas keeps [`MIN_MAIN`] whatever else
    // has to go; the strip is the first to yield and the command bar the
    // second, since the bar is the only thing on the page that takes a key.
    let rest = Rect::new(area.x, y.min(bottom), area.width, bottom.saturating_sub(y));
    let notice_h = u16::from(v.notice.is_some());
    let command_h = if rest.height >= MIN_MAIN + notice_h + 3 {
        3
    } else {
        0
    };
    let strip_h = if rest.height >= MIN_MAIN + notice_h + command_h + STRIP_H {
        STRIP_H
    } else {
        0
    };
    let [main, strip, notice_area, command] = Layout::vertical([
        Constraint::Fill(1),
        Constraint::Length(strip_h),
        Constraint::Length(notice_h),
        Constraint::Length(command_h),
    ])
    .areas(rest);
    let [left, _, right] = Layout::horizontal([
        Constraint::Fill(2),
        Constraint::Length(1),
        Constraint::Fill(1),
    ])
    .areas(main);
    canvas_rows = render_graph(left, buf, v, skin, &g);
    render_node(right, buf, v, skin, &g);

    if strip_h > 0 {
        let panels: [Rect; 5] = Layout::horizontal([Constraint::Fill(1); 5]).areas(strip);
        render_legend(panels[0], buf, skin, &g);
        render_summary(panels[1], buf, v, skin, &g);
        render_queues(panels[2], buf, v, skin);
        render_metrics(panels[3], buf, v, skin);
        render_filters(panels[4], buf, v, skin);
    }
    if let (Some(text), true) = (&v.notice, notice_h > 0) {
        let line = Line::from(Span::styled(
            fit(
                &dashes(text, skin.glyphs == ASCII),
                usize::from(notice_area.width),
                skin.glyphs.ellipsis,
            ),
            skin.palette.style(Role::Accent),
        ));
        buf.set_line(notice_area.x, notice_area.y, &line, notice_area.width);
    }
    if command_h > 0 {
        render_command(command, buf, v, skin);
    }
    canvas_rows
}

// -- the DAG canvas ----------------------------------------------------------

fn render_graph(area: Rect, buf: &mut Buffer, v: &GraphView, skin: &Skin, g: &PageGlyphs) -> u16 {
    if area.width < 8 || area.height < 3 {
        return 0;
    }
    let content = card_frame(area, buf, skin, "EXECUTION GRAPH (DAG)", Vec::new());
    if content.width == 0 || content.height < 2 {
        return 0;
    }
    let w = usize::from(content.width);

    // The footer, pinned to the panel's last inner row. Every key here is a
    // key the page answers: the row used to advertise a horizontal pan that
    // could not exist and four keys nothing was bound to, which is worse
    // than a short row, because a reader who tries them learns the page is
    // lying rather than that the feature is missing.
    let hints: [(String, &str); 6] = [
        (format!("[{}]", g.up_down), "Navigate"),
        ("[j/k]".into(), "Pan"),
        ("[PgUp/PgDn]".into(), "Page"),
        ("[Home/End]".into(), "Top/End"),
        ("[:]".into(), "Command"),
        ("[b]".into(), "Back"),
    ];
    let mut footer: Vec<Span<'static>> = Vec::new();
    for (i, (key, label)) in hints.into_iter().enumerate() {
        if i > 0 {
            footer.push(Span::raw("  "));
        }
        footer.push(Span::styled(key, skin.palette.style(Role::Accent)));
        footer.push(Span::styled(format!(" {label}"), skin.palette.dim()));
    }
    let canvas = Rect::new(content.x, content.y, content.width, content.height - 1);
    if v.nodes.is_empty() {
        let ascii = skin.glyphs == ASCII;
        set_row(
            content,
            content.height - 1,
            buf,
            Line::from(clip_spans(footer, w, skin)),
        );
        set_row(
            canvas,
            canvas.height / 2,
            buf,
            centered(&dashes(EMPTY_GRAPH, ascii), w, skin.palette.dim(), skin),
        );
        return canvas.height;
    }

    // The graph is laid out once at its full height and then a window of it
    // is blitted, so a box the viewport cuts in half is cut by the copy
    // rather than by arithmetic each painter would have to repeat. An
    // untouched cell is left alone, so whatever the frame painted under the
    // canvas survives.
    let rows = content_rows(&v.nodes).max(canvas.height);
    let scroll = v.scroll.min(rows.saturating_sub(canvas.height));
    let virt = Rect::new(canvas.x, 0, canvas.width, rows);
    let mut off = Buffer::empty(virt);
    let rects = layout(&v.nodes, virt, g);
    for (i, n) in v.nodes.iter().enumerate() {
        draw_box(&mut off, virt, rects[i], n, v.selected == Some(i), skin, g);
    }
    draw_edges(&mut off, virt, &rects, &v.nodes, skin, g);
    for row in 0..canvas.height {
        let sy = row + scroll;
        if sy >= rows {
            break;
        }
        for x in canvas.x..canvas.x + canvas.width {
            let cell = off[(x, sy)].clone();
            if cell != ratatui::buffer::Cell::EMPTY {
                buf[(x, canvas.y + row)] = cell;
            }
        }
    }

    // What the window does not reach, in both directions and recounted every
    // paint. A count that does not move while the view does is a lie, so
    // these are measured against the scrolled window, not against the graph.
    let above = rects.iter().filter(|r| r.y < scroll).count();
    let below = rects
        .iter()
        .filter(|r| r.y + r.height > scroll + canvas.height)
        .count();
    let mut right: Vec<Span<'static>> = Vec::new();
    if above > 0 {
        right.push(Span::styled(
            format!("{} {above} above", skin.glyphs.ellipsis),
            skin.palette.style(Role::Warn),
        ));
    }
    if below > 0 {
        if !right.is_empty() {
            right.push(Span::raw("  "));
        }
        right.push(Span::styled(
            format!("{} {below} below", skin.glyphs.ellipsis),
            skin.palette.style(Role::Warn),
        ));
    }
    set_row(content, content.height - 1, buf, lr(footer, right, w, skin));
    canvas.height
}

/// One node box: bordered, glyph + name bold, `id: nX` dim, status dot,
/// word and time. The border: accent when selected, green on the output
/// sink, else the status's own colour.
fn draw_box(
    buf: &mut Buffer,
    canvas: Rect,
    rect: Rect,
    n: &Node,
    selected: bool,
    skin: &Skin,
    g: &PageGlyphs,
) {
    let r = rect.intersection(canvas);
    if r.width < 2 || r.height < 2 {
        return;
    }
    let role = if selected {
        Role::Accent
    } else if n.kind == Kind::Output {
        Role::Ok
    } else {
        n.status.border_role()
    };
    let block = Block::bordered()
        .border_set(skin.glyphs.border)
        .border_style(skin.palette.style(role));
    let inner = block.inner(r);
    block.render(r, buf);
    if inner.width == 0 {
        return;
    }
    let ascii = skin.glyphs == ASCII;
    let rows = [
        Line::from(vec![
            Span::styled(
                format!("{} ", n.kind.glyph(g)),
                skin.palette.style(Role::Accent),
            ),
            Span::styled(n.name.clone(), skin.palette.bold(Role::Text)),
        ]),
        Line::from(Span::styled(format!("id: {}", n.id), skin.palette.dim())),
        Line::from(vec![
            Span::styled(
                format!("{} {}", n.status.glyph(g), n.status.word()),
                skin.palette.style(n.status.dot_role()),
            ),
            Span::styled(format!(" {}", dashes(&n.time, ascii)), skin.palette.dim()),
        ]),
    ];
    for (j, line) in rows.into_iter().enumerate() {
        set_row(inner, j as u16, buf, line);
    }
}

/// The dashed edges: a stub below the parent's bottom-center, a run along
/// the middle gap row, a vertical drop, and an arrowhead directly above the
/// child's top border. Arrowheads paint last so a crossing never eats one.
/// A Success Edge (child Running or Completed) is green and dim; a Pending
/// Edge is plain dim.
fn draw_edges(
    buf: &mut Buffer,
    canvas: Rect,
    rects: &[Rect],
    nodes: &[Node],
    skin: &Skin,
    g: &PageGlyphs,
) {
    let mut arrows: Vec<(u16, u16, Style)> = Vec::new();
    for (ci, n) in nodes.iter().enumerate() {
        for &p in n.parents.iter().filter(|&&p| p < ci) {
            let (pr, cr) = (rects[p], rects[ci]);
            let px = pr.x + pr.width / 2;
            let cx = cr.x + cr.width / 2;
            let style = if matches!(n.status, Status::Running | Status::Completed) {
                skin.palette.style(Role::Ok).add_modifier(Modifier::DIM)
            } else {
                skin.palette.dim()
            };
            let stub = pr.y + pr.height;
            let arrow = cr.y.saturating_sub(1);
            if arrow <= stub || cr.y == 0 {
                continue; // degenerate gap: no room for an edge
            }
            put(buf, canvas, px, stub, g.vdash, style);
            let mid = stub + 1;
            if mid < arrow {
                if px == cx {
                    put(buf, canvas, px, mid, g.vdash, style);
                } else {
                    for x in px.min(cx)..=px.max(cx) {
                        put(buf, canvas, x, mid, g.hdash, style);
                    }
                }
                for yy in (mid + 1)..arrow {
                    put(buf, canvas, cx, yy, g.vdash, style);
                }
            }
            arrows.push((cx, arrow, style));
        }
    }
    for (x, y, style) in arrows {
        put(buf, canvas, x, y, g.arrow, style);
    }
}

// -- the SELECTED NODE panel -------------------------------------------------

fn render_node(area: Rect, buf: &mut Buffer, v: &GraphView, skin: &Skin, g: &PageGlyphs) {
    if area.width < 6 || area.height < 3 {
        return;
    }
    let content = card_frame(area, buf, skin, "SELECTED NODE", Vec::new());
    if content.width == 0 || content.height == 0 {
        return;
    }
    let w = usize::from(content.width);
    let ascii = skin.glyphs == ASCII;
    let Some(si) = v.selected.filter(|&i| i < v.nodes.len()) else {
        set_row(content, 0, buf, empty_line(EMPTY_NODE, w, skin));
        return;
    };
    let node = &v.nodes[si];
    let d = v
        .details
        .get(si)
        .cloned()
        .or_else(|| v.detail.clone())
        .unwrap_or_default();
    let val = |s: &str| {
        if s.is_empty() {
            "–".to_string()
        } else {
            dashes(s, ascii)
        }
    };
    let mut i: u16 = 0;

    // The header: glyph + name bold, the live status flush right.
    set_row(
        content,
        i,
        buf,
        lr(
            vec![
                Span::styled(
                    format!("{} ", node.kind.glyph(g)),
                    skin.palette.style(Role::Accent),
                ),
                Span::styled(node.name.clone(), skin.palette.bold(Role::Text)),
            ],
            vec![Span::styled(
                format!("{} {}", node.status.glyph(g), node.status.word()),
                skin.palette.style(node.status.dot_role()),
            )],
            w,
            skin,
        ),
    );
    i += 2;

    for (label, value) in [
        ("Type", val(&d.node_type)),
        ("Node ID", node.id.clone()),
        ("Started", val(&d.started)),
        ("Latency", val(&d.latency)),
        ("Retries", val(&d.retries)),
        ("Worker", val(&d.worker)),
        ("Host", val(&d.host)),
        ("PID", val(&d.pid)),
    ] {
        set_row(
            content,
            i,
            buf,
            kv(
                label,
                Span::styled(value, skin.palette.style(Role::Accent)),
                w,
                skin,
            ),
        );
        i += 1;
    }

    // PARENT: the DAG's answer, "--- root ---" when there is none.
    i += 1;
    set_row(content, i, buf, section("PARENT", w, skin));
    i += 1;
    if node.parents.is_empty() {
        set_row(
            content,
            i,
            buf,
            Line::from(Span::styled("--- root ---".to_string(), skin.palette.dim())),
        );
        i += 1;
    } else {
        for &p in node.parents.iter().filter(|&&p| p < v.nodes.len()) {
            set_row(content, i, buf, relative_row(&v.nodes[p], w, skin, g));
            i += 1;
        }
    }

    // CHILDREN (n): also the DAG's answer.
    let children: Vec<usize> = v
        .nodes
        .iter()
        .enumerate()
        .filter(|(_, c)| c.parents.contains(&si))
        .map(|(j, _)| j)
        .collect();
    i += 1;
    set_row(
        content,
        i,
        buf,
        section(&format!("CHILDREN ({})", children.len()), w, skin),
    );
    i += 1;
    for c in children {
        set_row(content, i, buf, relative_row(&v.nodes[c], w, skin, g));
        i += 1;
    }

    for (title, rows) in [("INPUTS", &d.inputs), ("OUTPUTS", &d.outputs)] {
        i += 1;
        set_row(content, i, buf, section(title, w, skin));
        i += 1;
        if rows.is_empty() {
            set_row(
                content,
                i,
                buf,
                Line::from(Span::styled("–".to_string(), skin.palette.dim())),
            );
            i += 1;
        }
        for (label, value) in rows {
            set_row(
                content,
                i,
                buf,
                kv(
                    label,
                    Span::styled(val(value), skin.palette.style(Role::Accent)),
                    w,
                    skin,
                ),
            );
            i += 1;
        }
    }

    i += 1;
    set_row(content, i, buf, section("RECENT EVENTS", w, skin));
    i += 1;
    for e in &d.events {
        let level_style = match e.level {
            EventLevel::Info => skin.palette.style(Role::Info),
            EventLevel::Debug | EventLevel::Trace => skin.palette.dim(),
        };
        set_row(
            content,
            i,
            buf,
            Line::from(vec![
                Span::styled(format!("{}  ", e.time), skin.palette.dim()),
                Span::styled(format!("{:<5}  ", e.level.label()), level_style),
                Span::styled(
                    clip(&e.text, w.saturating_sub(cols(&e.time) + 9), skin),
                    skin.palette.style(Role::Text),
                ),
            ]),
        );
        i += 1;
    }
    if content.height > i {
        set_row(
            content,
            content.height - 1,
            buf,
            hint("t", "Tail Full Log", skin),
        );
    }
}

/// A parent or child row: id dim, name, the status in the mock's side-panel
/// vocabulary (✓ Completed, ↻ Running in amber) flush right.
fn relative_row(n: &Node, w: usize, skin: &Skin, g: &PageGlyphs) -> Line<'static> {
    let (glyph, role) = match n.status {
        Status::Completed => (g.check, Role::Ok),
        Status::Running => (g.kick, Role::Warn),
        s => (s.glyph(g), s.dot_role()),
    };
    lr(
        vec![
            Span::styled(format!("{}  ", n.id), skin.palette.dim()),
            Span::styled(n.name.clone(), skin.palette.style(Role::Text)),
        ],
        vec![Span::styled(
            format!("{glyph} {}", n.status.word()),
            skin.palette.style(role),
        )],
        w,
        skin,
    )
}

// -- the five-panel strip ----------------------------------------------------

fn render_legend(area: Rect, buf: &mut Buffer, skin: &Skin, g: &PageGlyphs) {
    if area.width < 6 || area.height < 3 {
        return;
    }
    let content = card_frame(area, buf, skin, "GRAPH LEGEND", Vec::new());
    let rows: [(String, Role, &str); 8] = [
        (g.dot.into(), Role::Ok, "Running"),
        (g.check.into(), Role::Ok, "Completed"),
        (g.dot.into(), Role::Warn, "Pending"),
        (g.dot.into(), Role::Info, "Queued"),
        (g.cross.into(), Role::Err, "Failed"),
        (g.skip.into(), Role::Dim, "Skipped"),
        (g.hdash.repeat(3), Role::Ok, "Success Edge"),
        (g.hdash.repeat(3), Role::Dim, "Pending Edge"),
    ];
    for (i, (glyph, role, label)) in rows.into_iter().enumerate() {
        set_row(
            content,
            i as u16,
            buf,
            Line::from(vec![
                Span::styled(format!("{glyph} "), skin.palette.style(role)),
                Span::styled(label.to_string(), skin.palette.style(Role::Text)),
            ]),
        );
    }
}

fn render_summary(area: Rect, buf: &mut Buffer, v: &GraphView, skin: &Skin, g: &PageGlyphs) {
    if area.width < 6 || area.height < 3 {
        return;
    }
    let content = card_frame(area, buf, skin, "RUN SUMMARY", Vec::new());
    let w = usize::from(content.width);
    let ascii = skin.glyphs == ASCII;
    let accent = skin.palette.style(Role::Accent);
    let dim = skin.palette.dim();
    let dash = || Span::styled(dashes("–", ascii), dim);
    let sm = v.summary.as_ref();
    let (done, total) = match sm.and_then(|s| s.progress) {
        Some((done, total)) => (done as usize, total as usize),
        None => completed_of(&v.nodes),
    };
    let pct = percent(done, total);
    let status = match sm {
        Some(s) => Span::styled(
            s.status.word().to_string(),
            skin.palette.style(s.status.dot_role()),
        ),
        None => dash(),
    };
    let val = |t: Option<&String>| match t {
        Some(t) => Span::styled(dashes(t, ascii), accent),
        None => dash(),
    };
    let rows: [(&str, Span<'static>); 6] = [
        ("Run ID", val(sm.map(|s| &s.run_id))),
        ("Status", status),
        ("Started", val(sm.map(|s| &s.started))),
        ("Elapsed", val(sm.map(|s| &s.elapsed))),
        (
            "Progress",
            Span::styled(format!("{done} / {total} ({pct}%)"), accent),
        ),
        ("ETA", val(sm.map(|s| &s.eta))),
    ];
    let mut i = 0u16;
    for (label, value) in rows {
        set_row(content, i, buf, kv(label, value, w, skin));
        i += 1;
    }
    // The block gauge: the status bar's idiom, ten segments, percent right.
    let filled = (pct * 10 / 100).min(10);
    let mut left: Vec<Span<'static>> = Vec::new();
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

fn render_queues(area: Rect, buf: &mut Buffer, v: &GraphView, skin: &Skin) {
    if area.width < 6 || area.height < 3 {
        return;
    }
    let content = card_frame(area, buf, skin, "QUEUE DEPTH", Vec::new());
    let w = usize::from(content.width);
    for (i, (label, depth)) in QUEUE_LABELS.iter().zip(v.queue_depths).enumerate() {
        set_row(
            content,
            i as u16,
            buf,
            kv(
                label,
                Span::styled(depth.to_string(), skin.palette.style(Role::Accent)),
                w,
                skin,
            ),
        );
    }
    if content.height > QUEUE_LABELS.len() as u16 {
        set_row(
            content,
            content.height - 1,
            buf,
            hint("w", "View Queues", skin),
        );
    }
}

fn render_metrics(area: Rect, buf: &mut Buffer, v: &GraphView, skin: &Skin) {
    if area.width < 6 || area.height < 3 {
        return;
    }
    let content = card_frame(area, buf, skin, "GRAPH METRICS", Vec::new());
    let w = usize::from(content.width);
    let ascii = skin.glyphs == ASCII;
    let accent = skin.palette.style(Role::Accent);
    let (done, total) = completed_of(&v.nodes);
    let pct = percent(done, total);
    let running = v
        .nodes
        .iter()
        .filter(|n| n.status == Status::Running)
        .count();
    let waiting = v
        .nodes
        .iter()
        .filter(|n| matches!(n.status, Status::Pending | Status::Queued))
        .count();
    let failed = v
        .nodes
        .iter()
        .filter(|n| n.status == Status::Failed)
        .count();
    let m = v.metrics.as_ref();
    let sup = |t: Option<&String>| dashes(t.map_or("–", |s| s.as_str()), ascii);
    let rows: [(&str, String); 10] = [
        ("Total Nodes", total.to_string()),
        ("Completed", format!("{done} ({pct}%)")),
        ("Running", running.to_string()),
        ("Pending / Queued", waiting.to_string()),
        ("Failed", failed.to_string()),
        (
            "Critical Path",
            m.map_or(0, |m| m.critical_path).to_string(),
        ),
        (
            "Parallel Branches",
            m.map_or(0, |m| m.parallel_branches).to_string(),
        ),
        ("Longest Task", sup(m.map(|m| &m.longest_task))),
        ("Avg Task Time", sup(m.map(|m| &m.avg_task_time))),
        ("Active Workers", sup(m.map(|m| &m.active_workers))),
    ];
    for (i, (label, value)) in rows.into_iter().enumerate() {
        set_row(
            content,
            i as u16,
            buf,
            kv(label, Span::styled(value, accent), w, skin),
        );
    }
    if content.height > 10 {
        set_row(
            content,
            content.height - 1,
            buf,
            hint("m", "More Metrics", skin),
        );
    }
}

fn render_filters(area: Rect, buf: &mut Buffer, v: &GraphView, skin: &Skin) {
    if area.width < 6 || area.height < 3 {
        return;
    }
    let content = card_frame(area, buf, skin, "FILTERS (f)", Vec::new());
    let w = usize::from(content.width);
    let ascii = skin.glyphs == ASCII;
    let accent = skin.palette.style(Role::Accent);
    let rows: [(&str, &String); 4] = [
        ("Status", &v.filters.status),
        ("Type", &v.filters.kind),
        ("Workers", &v.filters.workers),
        ("Search", &v.filters.search),
    ];
    for (i, (label, value)) in rows.into_iter().enumerate() {
        set_row(
            content,
            i as u16,
            buf,
            kv(label, Span::styled(dashes(value, ascii), accent), w, skin),
        );
    }
    if content.height > 4 {
        set_row(
            content,
            content.height - 1,
            buf,
            hint("r", "Reset Filters", skin),
        );
    }
}

/// The page command bar: accent border, prompt prefix, placeholder, the
/// execute hint.
fn render_command(area: Rect, buf: &mut Buffer, v: &GraphView, skin: &Skin) {
    if area.width < 4 || area.height < 3 {
        return;
    }
    let block = Block::bordered()
        .border_set(skin.glyphs.border)
        .border_style(skin.palette.style(Role::Accent));
    let inner = block.inner(area);
    block.render(area, buf);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let text = if v.query.is_empty() {
        Span::styled(
            if v.palette {
                COMMANDS.to_string()
            } else {
                PLACEHOLDER.to_string()
            },
            skin.palette.dim(),
        )
    } else {
        Span::styled(v.query.clone(), skin.palette.style(Role::Text))
    };
    // The mock's hint, verbatim and unconditional: the bar's own chrome is
    // pinned, so which keys it currently holds is said by the placeholder
    // above and by `[?]` (HELP_GRAPH), not by rewriting the mock's label.
    let line = lr(
        vec![
            Span::styled("> ".to_string(), skin.palette.bold(Role::Accent)),
            text,
        ],
        vec![Span::styled(HINT_EXECUTE.to_string(), skin.palette.dim())],
        usize::from(inner.width),
        skin,
    );
    buf.set_line(inner.x, inner.y, &line, inner.width);
}

// -- span carpentry ----------------------------------------------------------

/// How many nodes are Completed, of how many.
fn completed_of(nodes: &[Node]) -> (usize, usize) {
    (
        nodes
            .iter()
            .filter(|n| n.status == Status::Completed)
            .count(),
        nodes.len(),
    )
}

/// A whole truncated percent, and 0 of nothing is 0.
fn percent(done: usize, total: usize) -> usize {
    (done * 100).checked_div(total).unwrap_or(0)
}

/// One bordered panel with a header line, dim-bordered (selection lives on
/// the nodes here, not the panels). Returns the content area under the
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

/// A label left, its value flush right.
fn kv(label: &str, value: Span<'static>, w: usize, skin: &Skin) -> Line<'static> {
    lr(
        vec![Span::styled(
            label.to_string(),
            skin.palette.style(Role::Text),
        )],
        vec![value],
        w,
        skin,
    )
}

/// A section caption inside the node panel: dim, bold, upper-case already.
fn section(title: &str, w: usize, skin: &Skin) -> Line<'static> {
    Line::from(Span::styled(
        fit(title, w, skin.glyphs.ellipsis),
        skin.palette.dim().add_modifier(Modifier::BOLD),
    ))
}

/// A `[k] Label` affordance row.
fn hint(key: &str, label: &str, skin: &Skin) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("[{key}]"), skin.palette.style(Role::Accent)),
        Span::styled(format!(" {label}"), skin.palette.dim()),
    ])
}

/// One cell, bounds-checked against `clip`: the edge painter's pen.
fn put(buf: &mut Buffer, clip: Rect, x: u16, y: u16, sym: &str, style: Style) {
    if x >= clip.x && x < clip.x + clip.width && y >= clip.y && y < clip.y + clip.height {
        buf.set_string(x, y, sym, style);
    }
}

/// Left spans, a gap, right spans, at exactly `w` columns. The right side
/// survives and the left truncates — the sidebar's trailing-column ruling.
fn lr(left: Vec<Span<'static>>, right: Vec<Span<'static>>, w: usize, skin: &Skin) -> Line<'static> {
    // The right side is clipped first, and to the whole width: a value wider
    // than its panel used to survive whole, eat the label, and then be cut
    // mid-token by the buffer at the border. Cut with the ellipsis instead,
    // so a narrow panel says "there is more of this" rather than lying about
    // the value it shows.
    let right = clip_spans(right, w, skin);
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

/// Cut a span run to `w` display columns, marking the cut on the span the
/// budget ran out in.
fn clip_spans(spans: Vec<Span<'static>>, w: usize, skin: &Skin) -> Vec<Span<'static>> {
    if spans.iter().map(|s| cols(&s.content)).sum::<usize>() <= w {
        return spans;
    }
    let mut out: Vec<Span<'static>> = Vec::new();
    let mut used = 0usize;
    for s in spans {
        let room = w.saturating_sub(used);
        if room == 0 {
            break;
        }
        let t = clip(&s.content, room, skin);
        used += cols(&t);
        out.push(Span::styled(t, s.style));
    }
    out
}

/// A dim honest empty state, one line.
fn empty_line(text: &str, w: usize, skin: &Skin) -> Line<'static> {
    let ascii = skin.glyphs == ASCII;
    Line::from(Span::styled(
        fit(&dashes(text, ascii), w, skin.glyphs.ellipsis),
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

/// [`fit`], made total for budgets narrower than the ellipsis — the memory
/// page's `clip`, duplicated for the reason it documents.
fn clip(text: &str, budget: usize, skin: &Skin) -> String {
    if cols(text) <= budget {
        return text.to_string();
    }
    if budget < cols(skin.glyphs.ellipsis) {
        return ".".repeat(budget);
    }
    fit(text, budget, skin.glyphs.ellipsis)
}

/// A node's layer: 0 with no parents, else one past its deepest parent.
/// Only backward parent references count, so the walk terminates on any
/// input.
fn layers(nodes: &[Node]) -> Vec<u16> {
    let mut out: Vec<u16> = Vec::with_capacity(nodes.len());
    for (i, n) in nodes.iter().enumerate() {
        let l = n
            .parents
            .iter()
            .filter(|&&p| p < i)
            .map(|&p| out[p] + 1)
            .max()
            .unwrap_or(0);
        out.push(l);
    }
    out
}

/// Where every box goes inside `content`: layered top-down, uniform box
/// width from the widest content, siblings spread and centered, a layer that
/// cannot fit shrinking its boxes evenly. Returns one Rect per node, in node
/// order, un-clipped (the painter intersects with `content`).
fn layout(nodes: &[Node], content: Rect, g: &PageGlyphs) -> Vec<Rect> {
    let ls = layers(nodes);
    let n_layers = ls.iter().max().map_or(0, |&m| usize::from(m) + 1);
    let mut per_layer: Vec<Vec<usize>> = vec![Vec::new(); n_layers];
    for (i, &l) in ls.iter().enumerate() {
        per_layer[usize::from(l)].push(i);
    }
    // One box width for the whole graph: the widest of any node's three
    // content rows, plus its border.
    let want = nodes
        .iter()
        .map(|n| {
            (cols(n.kind.glyph(g)) + 1 + cols(&n.name))
                .max(4 + cols(&n.id))
                .max(cols(n.status.glyph(g)) + 1 + cols(n.status.word()) + 1 + cols(&n.time))
        })
        .max()
        .unwrap_or(0);
    let bw_max = u16::try_from(want + 2).unwrap_or(u16::MAX);

    let mut out = vec![Rect::default(); nodes.len()];
    for (l, members) in per_layer.iter().enumerate() {
        let n = members.len() as u16;
        if n == 0 {
            continue;
        }
        let gaps = (n - 1) * HGAP;
        // A layer that cannot fit at the uniform width shrinks its boxes
        // evenly; contents truncate, the trailing-column ruling.
        let bw = bw_max
            .min(content.width.saturating_sub(gaps) / n.max(1))
            .max(4);
        let total = n * bw + gaps;
        let x0 = content.x + content.width.saturating_sub(total) / 2;
        let y = content.y + (l as u16) * (BOX_H + VGAP);
        for (k, &i) in members.iter().enumerate() {
            out[i] = Rect::new(x0 + (k as u16) * (bw + HGAP), y, bw, BOX_H);
        }
    }
    out
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

    fn node(
        id: &str,
        name: &str,
        kind: Kind,
        status: Status,
        time: &str,
        parents: &[usize],
    ) -> Node {
        Node {
            id: id.into(),
            name: name.into(),
            kind,
            status,
            time: time.into(),
            parents: parents.to_vec(),
        }
    }

    /// The mock's sample DAG, in full: what the page looks like once the
    /// harness exists. The fidelity is pinned now.
    fn sample_nodes() -> Vec<Node> {
        vec![
            node(
                "n0",
                "orchestrator",
                Kind::Agent,
                Status::Running,
                "0.8s",
                &[],
            ),
            node(
                "n1",
                "planner",
                Kind::Agent,
                Status::Completed,
                "1.3s",
                &[0],
            ),
            node("n2", "coder", Kind::Agent, Status::Completed, "4.2s", &[1]),
            node(
                "n3",
                "reviewer",
                Kind::Agent,
                Status::Completed,
                "2.1s",
                &[1],
            ),
            node("n4", "tester", Kind::Agent, Status::Running, "1.4s", &[1]),
            node("n5", "shell", Kind::Tool, Status::Completed, "0.9s", &[1]),
            node(
                "n6",
                "file write",
                Kind::Tool,
                Status::Completed,
                "1.0s",
                &[2],
            ),
            node("n7", "git", Kind::Tool, Status::Completed, "1.2s", &[2, 3]),
            node(
                "n8",
                "test artifacts",
                Kind::Artifact,
                Status::Pending,
                "–",
                &[4],
            ),
            node("n9", "memory", Kind::Memory, Status::Queued, "–", &[5]),
            node(
                "n10",
                "output / artifacts",
                Kind::Output,
                Status::Pending,
                "–",
                &[6, 7, 8, 9],
            ),
        ]
    }

    fn populated() -> GraphView {
        GraphView {
            version: "v0.6.3".into(),
            nodes: sample_nodes(),
            selected: Some(0),
            details: Vec::new(),
            detail: Some(NodeDetail {
                node_type: "orchestrator".into(),
                started: "12:46:21".into(),
                latency: "0.8s".into(),
                retries: "0 / 3".into(),
                worker: "orchestrator@tokio".into(),
                host: "tokio".into(),
                pid: "42131".into(),
                inputs: vec![
                    ("Run Config".into(), "research-notes".into()),
                    ("Prompt".into(), "checkout-triage".into()),
                    ("Context Size".into(), "18,432 tokens".into()),
                    ("Memory Snapshot".into(), "v7 (3.2 KB)".into()),
                ],
                outputs: vec![
                    ("Plan".into(), "plan.md".into()),
                    ("Tasks".into(), "4".into()),
                    ("Artifacts".into(), "–".into()),
                ],
                events: vec![
                    Event {
                        time: "12:46:22".into(),
                        level: EventLevel::Info,
                        text: "planner spawned (n1)".into(),
                    },
                    Event {
                        time: "12:46:23".into(),
                        level: EventLevel::Debug,
                        text: "queue depth 2".into(),
                    },
                    Event {
                        time: "12:46:24".into(),
                        level: EventLevel::Info,
                        text: "coder completed (n2)".into(),
                    },
                    Event {
                        time: "12:46:25".into(),
                        level: EventLevel::Trace,
                        text: "heartbeat ok".into(),
                    },
                ],
            }),
            summary: Some(RunSummary {
                run_id: "jd93f2a1".into(),
                status: Status::Running,
                started: "12:46:21".into(),
                elapsed: "00:00:05".into(),
                eta: "~00:00:18".into(),
                progress: None,
            }),
            queue_depths: [2, 1, 0, 1, 2],
            metrics: Some(Metrics {
                critical_path: 5,
                parallel_branches: 3,
                longest_task: "coder (4.2s)".into(),
                avg_task_time: "1.6s".into(),
                active_workers: "4 / 4".into(),
            }),
            filters: Filters::default(),
            query: String::new(),
            palette: false,
            notice: None,
            scroll: 0,
            canvas_rows: 0,
        }
    }

    /// Today's truth: no harness, no run, nothing to show but the chrome.
    fn empty() -> GraphView {
        GraphView {
            version: "v0.6.3".into(),
            ..GraphView::default()
        }
    }

    fn buffer(v: &GraphView, w: u16, h: u16) -> Buffer {
        let area = Rect::new(0, 0, w, h);
        let mut buf = Buffer::empty(area);
        render(area, &mut buf, v, &skin());
        buf
    }

    fn lines(buf: &Buffer) -> Vec<String> {
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

    fn draw(v: &GraphView, w: u16, h: u16) -> Vec<String> {
        lines(&buffer(v, w, h))
    }

    /// Draw and store the canvas height back on the view, the way the shell
    /// does after a paint, so a test scrolls against the viewport the paint
    /// actually gave rather than one it guessed.
    fn draw_live(v: &mut GraphView, w: u16, h: u16) -> Vec<String> {
        let area = Rect::new(0, 0, w, h);
        let mut buf = Buffer::empty(area);
        let rows = render(area, &mut buf, v, &skin());
        v.canvas_rows = rows;
        v.scroll = v.scroll.min(max_scroll(v));
        lines(&buf)
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

    fn accent() -> Color {
        skin().palette.color(Role::Accent)
    }

    fn color(role: Role) -> Color {
        skin().palette.color(role)
    }

    /// The border Rect of the box whose id row reads `id: nX`, found from
    /// the buffer itself: the id text sits at the box's inner left, one row
    /// under the name row, two under the top border.
    fn box_of(buf: &Buffer, rows: &[String], id: &str) -> Rect {
        let (x, y) = locate(rows, &format!("id: {id}"));
        let left = x - 1;
        let top = y - 2;
        assert_eq!(
            buf[(left, top)].symbol(),
            "╭",
            "no top-left corner for {id}"
        );
        let mut right = left + 1;
        while buf[(right, top)].symbol() != "╮" {
            right += 1;
            assert!(right < buf.area().width, "no top-right corner for {id}");
        }
        Rect::new(left, top, right - left + 1, BOX_H)
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
        assert_eq!(rows[1], "Run Graph");
        assert_eq!(rows[2], SUBTITLE);
        assert!(
            rows[3].chars().all(|c| c == '─') && !rows[3].is_empty(),
            "rule missing"
        );
    }

    #[test]
    fn the_action_bar_is_the_mocks_exactly_and_live_is_green() {
        let buf = buffer(&populated(), 161, 75);
        let rows = lines(&buf);
        let bar = row_with(&rows, "[z] Zoom");
        assert_eq!(
            bar.trim_end(),
            "[z] Zoom ▾  [Z] Zoom ▴  [f] Filter  [t] Trace  [c] Center  [x] Collapse  [F] Follow (Live)"
        );
        let (lx, ly) = locate(&rows, "(Live)");
        assert_eq!(
            buf[(lx, ly)].style().fg,
            Some(color(Role::Ok)),
            "(Live) is not green"
        );
    }

    // -- the panels sit where the mock puts them ----------------------------

    #[test]
    fn the_dag_panel_is_left_and_the_node_panel_right() {
        let rows = draw(&populated(), 161, 75);
        let (gx, gy) = locate(&rows, "EXECUTION GRAPH (DAG)");
        let (nx, ny) = locate(&rows, "SELECTED NODE");
        assert_eq!(gy, ny, "the main pair does not share the header band");
        assert!(gx < 161 / 3, "DAG panel not on the left (x={gx})");
        assert!(nx > 161 * 3 / 5, "node panel not on the right (x={nx})");
    }

    // -- the DAG canvas ------------------------------------------------------

    #[test]
    fn every_sample_node_renders_glyph_name_id_and_status() {
        let rows = draw(&populated(), 161, 75);
        for n in sample_nodes() {
            row_with(&rows, &n.name);
            row_with(&rows, &format!("id: {}", n.id));
        }
        let g = page_glyphs(&skin());
        let name_row = row_with(&rows, "orchestrator");
        assert!(
            name_row.contains(&format!("{} orchestrator", g.agent)),
            "kind glyph missing: {name_row:?}"
        );
        let status = &rows[usize::from(locate(&rows, "id: n2").1) + 1];
        assert!(
            status.contains("✓ Completed 4.2s"),
            "status row wrong: {status:?}"
        );
        let pending = &rows[usize::from(locate(&rows, "id: n8").1) + 1];
        assert!(
            pending.contains("● Pending –"),
            "pending row wrong: {pending:?}"
        );
    }

    #[test]
    fn the_layout_layers_children_below_parents_and_never_overlaps() {
        let nodes = sample_nodes();
        let g = page_glyphs(&skin());
        for w in [40u16, 60, 78, 90, 105, 140, 200] {
            let content = Rect::new(0, 0, w, 60);
            let rects = layout(&nodes, content, &g);
            assert_eq!(rects.len(), nodes.len());
            for (i, n) in nodes.iter().enumerate() {
                for &p in &n.parents {
                    assert!(
                        rects[i].y > rects[p].y + rects[p].height,
                        "child {i} not strictly below parent {p} at w={w}"
                    );
                }
            }
            for i in 0..rects.len() {
                for j in i + 1..rects.len() {
                    assert!(
                        !rects[i].intersects(rects[j]),
                        "boxes {i} and {j} overlap at w={w}: {:?} {:?}",
                        rects[i],
                        rects[j]
                    );
                }
            }
            if w >= 50 {
                for (i, r) in rects.iter().enumerate() {
                    assert!(
                        r.x >= content.x && r.x + r.width <= content.x + content.width,
                        "box {i} out of the canvas at w={w}: {r:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn edges_connect_box_borders_with_stubs_and_arrowheads() {
        let buf = buffer(&populated(), 161, 75);
        let rows = lines(&buf);
        // Every non-root box wears an arrowhead directly above its top
        // border's center; every parent wears a stub directly below its
        // bottom border's center.
        for n in sample_nodes() {
            let b = box_of(&buf, &rows, &n.id);
            let cx = b.x + b.width / 2;
            if !n.parents.is_empty() {
                assert_eq!(
                    buf[(cx, b.y - 1)].symbol(),
                    "▼",
                    "no arrowhead above {}",
                    n.id
                );
            }
        }
        for parent in ["n0", "n1", "n2", "n3", "n4", "n5", "n6", "n7", "n8", "n9"] {
            let b = box_of(&buf, &rows, parent);
            let cx = b.x + b.width / 2;
            assert_eq!(
                buf[(cx, b.y + b.height)].symbol(),
                "╎",
                "no stub below {parent}"
            );
        }
    }

    #[test]
    fn success_edges_are_green_and_pending_edges_are_not() {
        let buf = buffer(&populated(), 161, 75);
        let rows = lines(&buf);
        let n1 = box_of(&buf, &rows, "n1"); // child Completed: success
        let a1 = buf[(n1.x + n1.width / 2, n1.y - 1)].style().fg;
        assert_eq!(a1, Some(color(Role::Ok)), "success edge not green");
        let n8 = box_of(&buf, &rows, "n8"); // child Pending: pending edge
        let a8 = buf[(n8.x + n8.width / 2, n8.y - 1)].style().fg;
        assert_ne!(a8, Some(color(Role::Ok)), "pending edge painted green");
    }

    #[test]
    fn the_selected_node_wears_the_accent_border_and_the_rest_their_own() {
        let buf = buffer(&populated(), 161, 75);
        let rows = lines(&buf);
        let b0 = box_of(&buf, &rows, "n0");
        assert_eq!(
            buf[(b0.x, b0.y)].style().fg,
            Some(accent()),
            "selected border not accent"
        );
        let b2 = box_of(&buf, &rows, "n2");
        assert_eq!(
            buf[(b2.x, b2.y)].style().fg,
            Some(color(Role::Dim)),
            "completed border not dim"
        );
        let b4 = box_of(&buf, &rows, "n4");
        assert_eq!(
            buf[(b4.x, b4.y)].style().fg,
            Some(color(Role::Warn)),
            "running border not amber"
        );
        let b9 = box_of(&buf, &rows, "n9");
        assert_eq!(
            buf[(b9.x, b9.y)].style().fg,
            Some(color(Role::Info)),
            "queued border not violet"
        );
        let b10 = box_of(&buf, &rows, "n10");
        assert_eq!(
            buf[(b10.x, b10.y)].style().fg,
            Some(color(Role::Ok)),
            "output border not green"
        );
    }

    #[test]
    fn the_dag_footer_names_only_keys_the_page_answers() {
        let rows = draw(&populated(), 161, 75);
        let footer = row_with(&rows, "Navigate");
        assert!(
            footer.contains(
                "[↑/↓] Navigate  [j/k] Pan  [PgUp/PgDn] Page  [Home/End] Top/End  [:] Command  [b] Back"
            ),
            "footer wrong: {footer:?}"
        );
        // The mock's row advertised a horizontal pan the layout cannot have
        // and four keys nothing was bound to. A footer that names a key the
        // page ignores teaches the reader the page is broken.
        for gone in [
            "Pan  [↑",
            "Focus Node",
            "Expand",
            "Focus Artifacts",
            "Live Tail",
        ] {
            assert!(
                !footer.contains(gone),
                "{gone:?} is still advertised: {footer:?}"
            );
        }
    }

    // -- the SELECTED NODE panel ---------------------------------------------

    #[test]
    fn the_node_panel_carries_the_mocks_kv_rows() {
        let rows = draw(&populated(), 161, 75);
        for (label, value) in [
            ("Type", "orchestrator"),
            ("Node ID", "n0"),
            ("Started", "12:46:21"),
            ("Latency", "0.8s"),
            ("Retries", "0 / 3"),
            ("Worker", "orchestrator@tokio"),
            ("Host", "tokio"),
            ("PID", "42131"),
        ] {
            let row = row_with(&rows, label);
            assert!(
                row.contains(value),
                "{value:?} missing beside {label:?}: {row:?}"
            );
        }
        let head = row_with(&rows, "SELECTED NODE");
        assert!(!head.is_empty());
        let title = &rows[usize::from(locate(&rows, "SELECTED NODE").1) + 1];
        assert!(
            title.contains("orchestrator"),
            "node header missing: {title:?}"
        );
        assert!(
            title.contains("● Running"),
            "node status missing: {title:?}"
        );
    }

    /// Parent and children derive from the DAG, so n0's panel says
    /// CHILDREN (1): the mock's side panel lists four while its own topology
    /// gives the orchestrator one child (design note). The mock's four-child
    /// idiom, ✓/↻ included, is pinned on n1, whose children really are four.
    #[test]
    fn parent_children_inputs_outputs_and_events_are_the_mocks() {
        let rows = draw(&populated(), 161, 75);
        let (_, py) = locate(&rows, "PARENT");
        assert!(
            rows[usize::from(py) + 1].contains("--- root ---"),
            "root marker missing"
        );
        row_with(&rows, "CHILDREN (1)");
        let planner = row_with(&rows, "n1  planner");
        assert!(
            planner.contains("✓ Completed"),
            "child status missing: {planner:?}"
        );

        let mut v = populated();
        v.selected = Some(1);
        let rows1 = draw(&v, 161, 75);
        let (_, p1) = locate(&rows1, "PARENT");
        assert!(
            rows1[usize::from(p1) + 1].contains("n0  orchestrator"),
            "parent row missing"
        );
        row_with(&rows1, "CHILDREN (4)");
        let tester = row_with(&rows1, "n4  tester");
        assert!(
            tester.contains("↻ Running"),
            "running child missing: {tester:?}"
        );

        let rows = draw(&populated(), 161, 75);
        row_with(&rows, "INPUTS");
        for (label, value) in [
            ("Run Config", "research-notes"),
            ("Prompt", "checkout-triage"),
            ("Context Size", "18,432 tokens"),
            ("Memory Snapshot", "v7 (3.2 KB)"),
            ("Plan", "plan.md"),
            ("Tasks", "4"),
        ] {
            let row = row_with(&rows, label);
            assert!(
                row.contains(value),
                "{value:?} missing beside {label:?}: {row:?}"
            );
        }
        row_with(&rows, "OUTPUTS");
        row_with(&rows, "RECENT EVENTS");
        let ev = row_with(&rows, "planner spawned (n1)");
        assert!(
            ev.contains("12:46:22") && ev.contains("INFO"),
            "event row wrong: {ev:?}"
        );
        row_with(&rows, "queue depth 2");
        let tail = row_with(&rows, "Tail Full Log");
        assert!(tail.contains("[t]"), "tail affordance keyless: {tail:?}");
    }

    // -- the five-panel strip ------------------------------------------------

    #[test]
    fn the_strip_panels_share_a_band_in_the_mocks_order() {
        let rows = draw(&populated(), 161, 75);
        let ys: Vec<(u16, u16)> = [
            "GRAPH LEGEND",
            "RUN SUMMARY",
            "QUEUE DEPTH",
            "GRAPH METRICS",
            "FILTERS (f)",
        ]
        .iter()
        .map(|n| locate(&rows, n))
        .collect();
        for pair in ys.windows(2) {
            assert_eq!(pair[0].1, pair[1].1, "strip headers not on one band");
            assert!(pair[0].0 < pair[1].0, "strip panels out of order");
        }
    }

    #[test]
    fn the_legend_names_every_status_and_both_edges() {
        let rows = draw(&populated(), 161, 75);
        let (_, ly) = locate(&rows, "GRAPH LEGEND");
        let below = &rows[usize::from(ly)..];
        for needle in [
            "● Running",
            "✓ Completed",
            "● Pending",
            "● Queued",
            "✗ Failed",
            "─ Skipped",
            "╌╌╌ Success Edge",
            "╌╌╌ Pending Edge",
        ] {
            assert!(
                below.iter().any(|r| r.contains(needle)),
                "{needle:?} missing from the legend"
            );
        }
    }

    #[test]
    fn the_run_summary_is_the_mocks_with_derived_progress() {
        let rows = draw(&populated(), 161, 75);
        for (label, value) in [
            ("Run ID", "jd93f2a1"),
            ("Status", "Running"),
            ("Elapsed", "00:00:05"),
            ("Progress", "6 / 11 (54%)"),
            ("ETA", "~00:00:18"),
        ] {
            let row = row_with(&rows, label);
            assert!(
                row.contains(value),
                "{value:?} missing beside {label:?}: {row:?}"
            );
        }
        let (_, sy) = locate(&rows, "RUN SUMMARY");
        let gauge = rows[usize::from(sy)..]
            .iter()
            .find(|r| r.contains('█'))
            .expect("summary gauge missing");
        assert!(gauge.contains('░'), "54% gauge not mixed: {gauge:?}");
    }

    #[test]
    fn the_queue_depths_are_the_mocks() {
        let rows = draw(&populated(), 161, 75);
        for (label, value) in QUEUE_LABELS.iter().zip(["2", "1", "0", "1", "2"]) {
            let row = row_with(&rows, label);
            assert!(
                row.contains(value),
                "{value:?} missing beside {label:?}: {row:?}"
            );
        }
        let row = row_with(&rows, "View Queues");
        assert!(row.contains("[w]"), "queue affordance keyless: {row:?}");
    }

    /// The count rows derive from the DAG (six completed of eleven; the
    /// mock's own RUN SUMMARY agrees at 6 / 11 (54%), its strip's
    /// "5 (45%)" is an arithmetic slip — design note).
    #[test]
    fn the_graph_metrics_derive_their_counts_from_the_dag() {
        let all = draw(&populated(), 161, 75);
        let (_, my) = locate(&all, "GRAPH METRICS");
        let rows: Vec<String> = all[usize::from(my)..].to_vec();
        for (label, value) in [
            ("Total Nodes", "11"),
            ("Completed", "6 (54%)"),
            ("Running", "2"),
            ("Pending / Queued", "3"),
            ("Failed", "0"),
            ("Critical Path", "5"),
            ("Parallel Branches", "3"),
            ("Longest Task", "coder (4.2s)"),
            ("Avg Task Time", "1.6s"),
            ("Active Workers", "4 / 4"),
        ] {
            let row = row_with(&rows, label);
            assert!(
                row.contains(value),
                "{value:?} missing beside {label:?}: {row:?}"
            );
        }
        let row = row_with(&rows, "More Metrics");
        assert!(row.contains("[m]"), "metrics affordance keyless: {row:?}");
    }

    #[test]
    fn the_filters_panel_is_the_mocks() {
        let rows = draw(&populated(), 161, 75);
        let (_, fy) = locate(&rows, "FILTERS (f)");
        let below = &rows[usize::from(fy)..];
        for needle in ["Status", "Type", "Workers", "Search"] {
            assert!(
                below.iter().any(|r| r.contains(needle)),
                "{needle:?} missing from filters"
            );
        }
        let row = row_with(&rows, "Reset Filters");
        assert!(row.contains("[r]"), "filters affordance keyless: {row:?}");
    }

    #[test]
    fn the_command_bar_is_the_mocks() {
        let rows = draw(&populated(), 161, 75);
        let q = row_with(&rows, PLACEHOLDER);
        let inner = q.trim_start_matches(['│', ' ']);
        assert!(inner.starts_with("> "), "prompt prefix missing: {q:?}");
        assert!(q.contains("[Enter] Execute"), "execute hint missing: {q:?}");
    }

    // -- the honest empty page ----------------------------------------------

    #[test]
    fn the_empty_page_keeps_the_chrome_and_tells_the_truth() {
        let rows = draw(&empty(), 161, 75);
        let (ex, _) = locate(&rows, EMPTY_GRAPH);
        assert!(ex > 10, "empty state not centered (x={ex})");
        row_with(&rows, EMPTY_NODE);
        row_with(&rows, "EXECUTION GRAPH (DAG)");
        row_with(&rows, "GRAPH LEGEND");
        row_with(&rows, "╌╌╌ Success Edge");
        let total = row_with(&rows, "Total Nodes");
        assert!(total.contains('0'), "total not a real zero: {total:?}");
        let progress = row_with(&rows, "Progress");
        assert!(
            progress.contains("0 / 0 (0%)"),
            "progress not honest: {progress:?}"
        );
        let run_id = row_with(&rows, "Run ID");
        assert!(run_id.contains('–'), "run id not a dash: {run_id:?}");
        for label in QUEUE_LABELS {
            let row = row_with(&rows, label);
            assert!(row.contains('0'), "{label} not zero: {row:?}");
        }
    }

    #[test]
    fn the_empty_page_makes_no_false_claims() {
        let all = draw(&empty(), 161, 75).join("\n");
        for sample in [
            "orchestrator",
            "jd93f2a1",
            "id: n0",
            "12:46:21",
            "coder",
            "4.2s",
            "00:00:05",
            "42131",
            "▼",
        ] {
            assert!(!all.contains(sample), "sample data leaked: {sample}");
        }
        // The summary's Status value: the cell flush against the panel's
        // right border on the row under Run ID (its own panel; the legend's
        // "● Running" shares terminal rows with it and proves nothing).
        let buf = buffer(&empty(), 161, 75);
        let rows = lines(&buf);
        let (rx, ry) = locate(&rows, "Run ID");
        let mut border = rx;
        while buf[(border, ry)].symbol() != "│" {
            border += 1;
        }
        assert_eq!(
            buf[(border - 1, ry + 1)].symbol(),
            "–",
            "status value is not a dash"
        );
    }

    // -- invariants ----------------------------------------------------------

    #[test]
    fn no_row_is_ever_wider_than_the_area() {
        for v in [populated(), empty()] {
            for (w, h) in [
                (120u16, 75u16),
                (130, 60),
                (140, 75),
                (161, 75),
                (180, 60),
                (200, 75),
                (80, 30),
                (40, 12),
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
        let area = Rect::new(0, 0, 120, 75);
        let mut buf = Buffer::empty(area);
        render(
            area,
            &mut buf,
            &populated(),
            &Skin::new(Palette::new(Level::Ansi16), ASCII),
        );
        for y in 0..75u16 {
            let row: String = (0..120u16)
                .map(|x| buf[(x, y)].symbol().to_string())
                .collect();
            assert!(row.is_ascii(), "non-ASCII under the ASCII skin: {row:?}");
        }
    }

    /// Eyeball dump: `cargo test -p emma the_populated_graph -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn the_populated_graph_at_161x75_for_eyeballing() {
        for row in draw(&populated(), 161, 75) {
            println!("{row}");
        }
    }

    // -- panels that hold their borders --------------------------------------

    /// The panel borders, unbroken. A panel's top row fixes the columns its
    /// two verticals live in; every row down to its bottom must still hold a
    /// `│` there. Text that overran its panel would have painted over one,
    /// which is the defect this pins.
    ///
    /// It runs at the widths the page is actually opened at, narrow ones
    /// included, because a narrow panel is where a value that is not cut runs
    /// out of room.
    #[test]
    fn nothing_paints_on_or_past_a_panel_border() {
        for (w, h) in [
            (161u16, 75u16),
            (120, 60),
            (90, 40),
            (70, 30),
            (58, 34),
            (46, 20),
        ] {
            for v in [populated(), empty()] {
                let rows: Vec<Vec<char>> = draw(&v, w, h).iter().map(|r| by_column(r)).collect();
                for (y, top) in rows.iter().enumerate() {
                    if top.first() != Some(&'╭') {
                        continue;
                    }
                    let sides: Vec<usize> = top
                        .iter()
                        .enumerate()
                        .filter(|(_, &c)| c == '╭' || c == '╮')
                        .map(|(x, _)| x)
                        .collect();
                    for row in rows.iter().skip(y + 1) {
                        if row.first() == Some(&'╰') {
                            break;
                        }
                        if row.first() != Some(&'│') {
                            break;
                        }
                        for &x in &sides {
                            assert_eq!(
                                row.get(x).copied(),
                                Some('│'),
                                "a border column was painted over at {w}x{h}: {:?}",
                                row.iter().collect::<String>(),
                            );
                        }
                    }
                }
            }
        }
    }

    /// A rendered row as one char per display column, so a column index means
    /// the same thing on every row whatever glyphs are on it.
    fn by_column(row: &str) -> Vec<char> {
        let mut out = Vec::new();
        for c in row.chars() {
            out.push(c);
            out.extend(std::iter::repeat_n(' ', cols(&c.to_string()).max(1) - 1));
        }
        out
    }

    /// A value too wide for its panel is cut with the ellipsis rather than
    /// left whole to eat its label and be sliced at the border by the buffer.
    /// RUN SUMMARY's Progress row is the one that used to do it.
    #[test]
    fn a_value_wider_than_its_panel_is_cut_with_the_ellipsis() {
        let skin = skin();
        let line = lr(
            vec![Span::raw("Progress")],
            vec![Span::raw("6 / 11 (54%)")],
            9,
            &skin,
        );
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(
            cols(&text),
            9,
            "a row must be exactly its panel's inner width: {text:?}"
        );
        assert!(
            text.contains('…'),
            "a cut value must say it was cut: {text:?}"
        );
        // And on the page, where the panel is genuinely too narrow for it.
        let rows = draw(&populated(), 58, 34);
        let progress = rows
            .iter()
            .find(|r| r.contains("6 / 11"))
            .expect("the progress row is on screen");
        assert!(progress.contains('…'), "{progress:?}");
    }

    /// The canvas keeps its floor when the window is short: the strip yields
    /// first and the command bar second, so the page never becomes head rows
    /// hanging over a panel strip with no graph under them.
    #[test]
    fn a_short_window_keeps_the_graph_panel_rather_than_the_strip() {
        let rows = draw(&populated(), 120, 20);
        row_with(&rows, "EXECUTION GRAPH (DAG)");
        row_with(&rows, "SELECTED NODE");
        assert!(
            !rows.iter().any(|r| r.contains("GRAPH LEGEND")),
            "the strip must yield before the canvas does",
        );
        // And the head rows still have a panel under them rather than air.
        let (_, title) = locate(&rows, "Run Graph");
        assert!(
            rows[usize::from(title) + 1..]
                .iter()
                .any(|r| r.starts_with('╭')),
            "no panel under the head: {rows:?}",
        );
    }

    /// Every left-panel text row starts strictly inside the panel border,
    /// with the border in column 0 and the text one cell in.
    #[test]
    fn left_panel_text_sits_inside_the_border() {
        for (w, h) in [(161u16, 75u16), (90, 40)] {
            let rows = draw(&populated(), w, h);
            for label in ["EXECUTION GRAPH (DAG)", "GRAPH LEGEND", "● Running"] {
                let (x, _) = locate(&rows, label);
                assert!(x >= 1, "{label:?} sits on the border at {w}x{h} (x={x})");
            }
        }
    }

    // -- the command bar, wired ----------------------------------------------

    /// The commands the page can honestly serve act; the ones it cannot name
    /// what is missing. Both consume the query, so the bar can never show a
    /// command that has already run.
    #[test]
    fn the_palette_acts_on_what_it_can_and_names_what_it_cannot() {
        let mut v = populated();
        v.query = "node n3".into();
        command(&mut v);
        assert_eq!(v.selected, Some(3));
        assert_eq!(v.notice.as_deref(), Some("selected n3"));
        assert!(v.query.is_empty(), "a run command clears the bar");

        v.filters.status = "Failed".into();
        v.query = "clear".into();
        command(&mut v);
        assert_eq!(v.filters.status, "All");
        assert_eq!(v.selected, Some(0));
        assert_eq!(v.notice.as_deref(), Some(NOTICE_CLEARED));

        v.query = "metrics".into();
        command(&mut v);
        assert_eq!(v.notice.as_deref(), Some(NOTICE_METRICS));

        v.query = "run".into();
        command(&mut v);
        assert_eq!(v.notice.as_deref(), Some(NOTICE_RUN));

        v.query = "inspect".into();
        command(&mut v);
        let notice = v.notice.clone().expect("inspect answers about a node");
        assert!(notice.starts_with("n0"), "{notice:?}");

        v.query = "teleport".into();
        command(&mut v);
        let notice = v.notice.clone().expect("an unknown command still answers");
        assert!(notice.contains(COMMANDS), "{notice:?}");
    }

    /// An empty graph has no node to select, and says so instead of panicking
    /// or pretending.
    #[test]
    fn the_palette_on_an_empty_graph_says_there_is_nothing_to_inspect() {
        let mut v = empty();
        v.query = "inspect".into();
        command(&mut v);
        assert_eq!(v.notice.as_deref(), Some(NOTICE_NO_NODE));
        assert_eq!(v.selected, None);
    }

    /// The notice renders above the command bar and is cut to the page width.
    #[test]
    fn a_notice_renders_above_the_command_bar() {
        let mut v = populated();
        v.notice = Some(NOTICE_RUN.to_string());
        let rows = draw(&v, 161, 75);
        let (_, notice) = locate(&rows, "read-only");
        let (_, bar) = locate(&rows, PLACEHOLDER);
        assert!(notice < bar, "the notice must sit above the bar");
    }

    /// Selection wraps in both directions and an empty graph has none to move.
    #[test]
    fn navigate_wraps_and_an_empty_graph_stays_unselected() {
        let mut v = populated();
        v.selected = Some(v.nodes.len() - 1);
        navigate(&mut v, true);
        assert_eq!(v.selected, Some(0));
        navigate(&mut v, false);
        assert_eq!(v.selected, Some(v.nodes.len() - 1));
        let mut e = empty();
        navigate(&mut e, true);
        assert_eq!(e.selected, None);
    }

    /// The node panel follows the selection when the caller supplied a detail
    /// per node, and falls back to the single `detail` when it did not.
    #[test]
    fn the_node_panel_reads_the_selected_nodes_own_detail() {
        let mut v = populated();
        v.details = v
            .nodes
            .iter()
            .map(|n| NodeDetail {
                node_type: format!("type of {}", n.id),
                ..NodeDetail::default()
            })
            .collect();
        v.selected = Some(2);
        let rows = draw(&v, 161, 75);
        let ty = row_with(&rows, "Type");
        assert!(
            ty.contains("type of n2"),
            "the panel showed another node: {ty:?}"
        );
        // No per-node details is the mock's shape, unchanged.
        v.details.clear();
        let rows = draw(&v, 161, 75);
        assert!(row_with(&rows, "Type").contains("orchestrator"));
    }

    /// A recorded fraction is about the work and beats the DAG's own
    /// completed-of-total, gauge included. Without one the DAG still answers.
    #[test]
    fn a_recorded_progress_fraction_wins_over_the_node_count() {
        let mut v = populated();
        if let Some(s) = v.summary.as_mut() {
            s.progress = Some((4, 7));
        }
        let rows = draw(&v, 161, 75);
        let row = row_with(&rows, "Progress");
        assert!(row.contains("4 / 7 (57%)"), "{row:?}");
        let gauge = rows
            .iter()
            .find(|r| r.contains('█') && r.contains("57%"))
            .expect("the gauge must move with the fraction");
        assert!(
            gauge.contains("░"),
            "a part-full gauge keeps its empty half: {gauge:?}"
        );
        if let Some(s) = v.summary.as_mut() {
            s.progress = None;
        }
        assert!(row_with(&draw(&v, 161, 75), "Progress").contains("6 / 11 (54%)"));
    }

    /// A chain is taller than any terminal and the page has no pan key, so a
    /// node below the fold must be counted rather than clipped in silence.
    #[test]
    fn nodes_below_the_fold_are_counted_on_the_footer() {
        let rows = draw(&populated(), 161, 30);
        let footer = row_with(&rows, "Navigate");
        assert!(
            footer.contains('…') && footer.contains("below"),
            "the hidden nodes are uncounted: {footer:?}",
        );
        // Nothing is hidden the other way at the top of the graph.
        assert!(
            !footer.contains("above"),
            "unscrolled and yet something is above: {footer:?}"
        );
        // With room for all of them the count is absent, not a zero.
        let tall = draw(&populated(), 161, 120);
        let full = row_with(&tall, "Navigate");
        assert!(
            !full.contains("below") && !full.contains("above"),
            "a fitting graph counted: {full:?}"
        );
    }

    // -- the viewport --------------------------------------------------------

    /// The defect the owner reported: navigating to a node below the fold
    /// moved the inspector and left the canvas where it was, so the node
    /// being described stayed off screen.
    #[test]
    fn selecting_a_node_below_the_fold_scrolls_it_into_view() {
        let mut v = populated();
        v.canvas_rows = 20; // two layers and a gap: n0 through n2 at most
        v.selected = Some(0);
        // n5 is on layer 3, whose top row is 3 * (BOX_H + VGAP) = 24, well
        // past a 20-row canvas.
        for _ in 0..5 {
            navigate(&mut v, true);
        }
        assert_eq!(v.selected, Some(5));
        let top = layers(&v.nodes)[5] * (BOX_H + VGAP);
        assert!(
            v.scroll <= top && top + BOX_H <= v.scroll + v.canvas_rows,
            "node 5 (rows {top}..{}) is outside the window at scroll {}",
            top + BOX_H,
            v.scroll,
        );
    }

    /// And the other way: coming back up drags the viewport back.
    #[test]
    fn selecting_a_node_above_the_window_scrolls_back_up() {
        let mut v = populated();
        v.canvas_rows = 20;
        v.selected = Some(9);
        v.scroll = max_scroll(&v);
        assert!(
            v.scroll > 0,
            "the fixture must not fit, or this proves nothing"
        );
        v.selected = Some(0);
        navigate(&mut v, false); // wraps to the last node, then back to 0
        navigate(&mut v, true);
        assert_eq!(v.selected, Some(0));
        assert_eq!(v.scroll, 0, "the first node did not bring the top back");
    }

    /// A node already inside the window does not move it: the viewport
    /// follows the selection by the least distance, not by recentering.
    #[test]
    fn a_visible_selection_leaves_the_viewport_alone() {
        let mut v = populated();
        v.canvas_rows = 20;
        v.scroll = 6;
        v.selected = Some(1);
        reveal_selected(&mut v);
        assert_eq!(v.scroll, 6);
    }

    #[test]
    fn pan_clamps_at_the_top_and_at_the_last_row() {
        let mut v = populated();
        v.canvas_rows = 20;
        let max = max_scroll(&v);
        assert!(max > 0);
        pan(&mut v, false, 5);
        assert_eq!(v.scroll, 0, "panning up from the top moved");
        pan(&mut v, true, 1_000);
        assert_eq!(v.scroll, max, "panning down ran past the last node");
        pan(&mut v, true, 1);
        assert_eq!(v.scroll, max);
    }

    #[test]
    fn pan_to_jumps_to_the_top_and_to_the_end() {
        let mut v = populated();
        v.canvas_rows = 20;
        pan_to(&mut v, false);
        assert_eq!(v.scroll, max_scroll(&v));
        pan_to(&mut v, true);
        assert_eq!(v.scroll, 0);
    }

    /// A graph that fits has nowhere to go, whatever a key asks for.
    #[test]
    fn a_graph_that_fits_cannot_scroll() {
        let mut v = populated();
        v.canvas_rows = content_rows(&v.nodes) + 10;
        assert_eq!(max_scroll(&v), 0);
        pan(&mut v, true, 9);
        assert_eq!(v.scroll, 0);
    }

    /// The paint reports the canvas height, which is the fact the pure key
    /// handler cannot measure for itself.
    #[test]
    fn render_reports_the_canvas_height() {
        let area = Rect::new(0, 0, 161, 40);
        let mut buf = Buffer::empty(area);
        let rows = render(area, &mut buf, &populated(), &skin());
        assert!(rows > 0 && rows < 40, "implausible canvas height: {rows}");
    }

    /// The viewport actually moves the paint: a node hidden below the fold
    /// is on screen once the canvas is scrolled to it, and the node that was
    /// at the top is gone.
    #[test]
    fn scrolling_paints_a_different_window_of_the_graph() {
        let mut v = populated();
        let before = draw_live(&mut v, 161, 30);
        assert!(
            before.iter().any(|r| r.contains("id: n0")),
            "n0 should start visible"
        );
        assert!(
            !before.iter().any(|r| r.contains("id: n9")),
            "n9 should start below the fold"
        );
        pan_to(&mut v, false);
        let after = draw_live(&mut v, 161, 30);
        assert!(
            after.iter().any(|r| r.contains("id: n9")),
            "the last node never came into view"
        );
        assert!(
            !after.iter().any(|r| r.contains("id: n0")),
            "the first node is still painted"
        );
    }

    /// Both directions, counted against the window rather than the graph.
    #[test]
    fn a_scrolled_canvas_counts_above_and_below() {
        let mut v = populated();
        draw_live(&mut v, 161, 30);
        pan(&mut v, true, 12);
        let mid = draw_live(&mut v, 161, 30);
        let footer = row_with(&mid, "Navigate");
        assert!(
            footer.contains("above") && footer.contains("below"),
            "mid-scroll footer: {footer:?}"
        );
        // At the end there is nothing below, and something above.
        pan_to(&mut v, false);
        let end = draw_live(&mut v, 161, 30);
        let footer = row_with(&end, "Navigate");
        assert!(
            footer.contains("above"),
            "no count above at the end: {footer:?}"
        );
        assert!(
            !footer.contains("below"),
            "something is still below the last row: {footer:?}"
        );
    }

    /// The count is not a constant: scrolling one row moves it.
    #[test]
    fn the_below_count_moves_with_the_viewport() {
        let count = |v: &mut GraphView| {
            let rows = draw_live(v, 161, 30);
            row_with(&rows, "Navigate")
                .split("… ")
                .find(|s| s.contains("below"))
                .and_then(|s| s.split_whitespace().next())
                .and_then(|n| n.parse::<usize>().ok())
                .unwrap_or(0)
        };
        let mut v = populated();
        let first = count(&mut v);
        pan(&mut v, true, 16);
        let later = count(&mut v);
        assert!(
            first > 0 && later < first,
            "the count stood still: {first} then {later}"
        );
    }
}
