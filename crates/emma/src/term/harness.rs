//! The Harness dashboard: the owner's mock, cell for cell.
//!
//! The mock is the acceptance criterion (the Harness page design), the
//! standing rule Settings and Memory were built under: action bar, six cards
//! in a two-column grid (ACTIVE RUNS, WORKER POOL, RUNTIME STATUS, TASK
//! QUEUE, TOOL GATES, RESOURCES), a full-width band holding EVENT LOG and
//! TRACE / CONTEXT, and the page's own command-palette input bar, with every
//! label, glyph and affordance from the mock.
//!
//! The one sanctioned divergence is data. The harness the mock describes
//! (worker pool, task queue, tool gates) is aspirational — plan-harness.md
//! stages the real feeds as HB1..HB4 — and rendering its sample runs as if
//! they existed would be false chrome, the status bar law page-sized. So
//! everything renders from a [`HarnessView`]: a populated view reproduces
//! the mock exactly (the tests pin that now, with the mock's sample data),
//! and an empty view keeps the exact card chrome with honest one-line dim
//! empty states and real zeros. RUNTIME STATUS is the exception that proves
//! the rule: binary, version and workspace are real today and render live.
//!
//! The page is live (harness-live, 2026-08-26), and since the port of the
//! macOS fork it is a **process manager** rather than a reader: `[p]`/`[r]`/
//! `[x]` signal a run through [`crate::runctl`], `[a]`/`[+]` start one,
//! `[A]` archives a transcript, `[D]` deletes one after a second press,
//! `[n]`/`[c]`/⇧↑↓ edit the task file, and `[t]`/`[s]`/`[m]`/`[d]` write the
//! next run's tool policy. [`handle_key`] is still the pure key seam
//! (memory's M5 pattern): it names what to do and the shell
//! ([`super::app::App::harness_key`]) is the only half that touches a
//! process or a file. [`PageMode`] mounts Inspect, Run Graph and ALL RUNS
//! inside this one occupant. The action bar renders as clickable chips and
//! the paint reports its control rects ([`Hits`]) for the shell's hit-test —
//! every card footer's chips as well as the bar's, so clicking `[A] Archive`
//! is the same dispatch as pressing it.
//!
//! **Where a control does not exist on this platform, the page says so and
//! the key still answers.** [`crate::runctl::supported`] is asked before the
//! action bar is painted: an unavailable control's chip is dim rather than
//! accent, the bar carries [`no_controls_note`] naming the platform in
//! words, and pressing the key sets the refusal's own sentence as the notice
//! without ever asking the shell for a signal. That is `bindings.rs`'s rule
//! and `usertools.rs`'s `Tool::routed` precedent: a rendered key that does
//! nothing is the defect, and a `cfg` that answers `true` where the platform
//! answers nothing is the worse version of it. On Windows the three signals
//! are the whole of what is missing — archive, delete, launch and the policy
//! write all have real Windows implementations in `runctl`.
//!
//! Pure rendering and pure key handling, like [`super::memory`]: the shell
//! ([`super::app::App::harness_key`]) owns the disk. All width arithmetic
//! is in display columns via [`cols`]/[`fit`], and no row ever writes past
//! its card's inner width: the trailing column survives and the text
//! truncates, the sidebar's ruling. The input bar and the notice row are
//! bottom-anchored like memory's query box, and cards compress honestly
//! (`… N more`) when the window runs short.

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

/// One run's lifecycle state. Decides the status word, its color, and the
/// verb on the second line (`started` / `paused` / `completed`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RunState {
    #[default]
    Running,
    Paused,
    Completed,
    /// Not in the mock — the mock's sample day had no failures. Real history
    /// does, and rendering one as Completed would be the lie this page's
    /// no-false-claims test exists to prevent.
    Failed,
}

/// One ACTIVE RUNS mini-box.
#[derive(Debug, Clone, Default)]
pub struct Run {
    pub name: String,
    pub state: RunState,
    /// The run id (`jd93f2a1` shapes). Empty renders the second line without
    /// the `id · ` prefix — the mock shows the id only on the first run.
    pub id: String,
    /// The run's full stable id (`<session>#<n>` or a `sub_id`) — what
    /// [`HarnessAction::Inspect`] carries to `harness_state::detail`. The
    /// display `id` above is a tail cut and cannot find the run again.
    pub key: String,
    /// Clock text for the second line, caller-formatted (`10:21:34`).
    pub stamp: String,
    pub done: u32,
    pub total: u32,
}

/// One WORKER POOL row.
#[derive(Debug, Clone)]
pub struct Worker {
    pub name: String,
    /// The dim sub-label (`tokio`, `rust-analyzer`, ...).
    pub sub: String,
    pub online: bool,
    /// Latency text, caller-formatted (`0.8s`).
    pub latency: String,
}

/// A TASK QUEUE row's state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskState {
    Running,
    Pending,
    /// Not in the mock, whose sample queue had nothing finished in it. A real
    /// task file does, and `[c] Clear Done` is a control over exactly these
    /// rows: hiding them would make that key act on invisible data.
    Done,
}

/// One TASK QUEUE row. `n` is caller-supplied, not the row index: the mock
/// numbers its last visible task 9 with rows 6-8 scrolled out of view, so
/// position in the queue is data, not geometry.
#[derive(Debug, Clone)]
pub struct Task {
    pub n: u32,
    pub name: String,
    /// The task file's own handle for this row (`#abc123`), which is what a
    /// mutation names. The displayed `n` is a position and cannot find the
    /// task again after a reorder, exactly as `Run::id` cannot find a run.
    pub id: String,
    pub state: TaskState,
    /// Whole-percent progress; `None` renders the en-dash placeholder.
    pub progress_pct: Option<u8>,
}

/// A TOOL GATES row's status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateStatus {
    Allowed,
    Pending,
}

/// One TOOL GATES / APPROVALS row.
#[derive(Debug, Clone)]
pub struct Gate {
    pub tool: String,
    pub request: String,
    pub status: GateStatus,
}

/// An EVENT LOG row's level. Decides the level column's color only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    Info,
    Warn,
    Error,
}

/// One EVENT LOG row.
#[derive(Debug, Clone)]
pub struct Event {
    pub time: String,
    pub level: LogLevel,
    pub text: String,
}

/// The RUNTIME STATUS card's values. Binary, version and workspace are real
/// today (the shell knows all three); every other field is empty until a
/// backend stage feeds it, and an empty string renders as the honest dash.
#[derive(Debug, Clone, Default)]
pub struct RuntimeView {
    pub binary: String,
    pub version: String,
    pub runtime: String,
    pub workspace: String,
    pub mode: String,
    pub model: String,
    pub provider: String,
    pub started: String,
    pub uptime: String,
    /// `None` renders the dash — health is a claim, not chrome.
    pub health: Option<bool>,
}

/// The RESOURCES card's values. Strings are caller-formatted and an empty
/// one renders as the honest dash; the two `_pct` fields drive the gauges.
#[derive(Debug, Clone, Default)]
pub struct ResourcesView {
    pub cpu_pct: Option<u8>,
    pub mem: String,
    pub mem_pct: Option<u8>,
    pub disk_io: String,
    pub network: String,
    pub workers: String,
    pub queue_depth: String,
    pub avg_latency: String,
    pub p95_latency: String,
    pub retries: String,
    pub span_rate: String,
    /// The header's right-hand `Live` claim; only render it when true.
    pub live: bool,
}

impl ResourcesView {
    /// Whether every row of this card has a value behind it.
    ///
    /// The `Live` badge is a claim about the whole card, so a card with one
    /// placeholder in it does not get to wear it. Callers set `live` from
    /// this rather than deciding by hand, which is what stops the badge and
    /// the rows from drifting apart.
    pub fn complete(&self) -> bool {
        self.cpu_pct.is_some()
            && self.mem_pct.is_some()
            && ![
                &self.mem,
                &self.disk_io,
                &self.network,
                &self.workers,
                &self.queue_depth,
                &self.avg_latency,
                &self.p95_latency,
                &self.retries,
                &self.span_rate,
            ]
            .iter()
            .any(|s| s.is_empty())
    }
}

/// The TRACE / CONTEXT panel's values, all caller-formatted; empty renders
/// the dash.
#[derive(Debug, Clone, Default)]
pub struct TraceView {
    pub trace_id: String,
    pub parent_span: String,
    pub total_spans: String,
    pub active_spans: String,
    pub last_span: String,
    pub context_size: String,
    pub prompt_cache: String,
}

/// What the page shows. Today almost every field defaults empty because no
/// harness backend exists (plan HB1..HB4); RUNTIME STATUS's live trio is the
/// exception.
#[derive(Debug, Clone, Default)]
pub struct HarnessView {
    /// The running binary's version, `v`-prefixed — the mock's top-right corner.
    pub version: String,
    pub runs: Vec<Run>,
    /// The selected ACTIVE RUNS mini-box, whose border takes the accent.
    /// Render-only: cycling it with keys is the integration stage's.
    pub selected_run: Option<usize>,
    pub workers: Vec<Worker>,
    pub runtime: RuntimeView,
    pub tasks: Vec<Task>,
    /// The selected TASK QUEUE row, worn as the chip band (the mock's row 4).
    pub selected_task: Option<usize>,
    /// The queue header's `Depth: N`. A view field, not `tasks.len()`: the
    /// mock shows Depth: 5 over a queue whose visible rows number to 9.
    pub queue_depth: u64,
    /// The gates header's `Auto-approve: X` value; empty renders the dash.
    pub auto_approve: String,
    pub gates: Vec<Gate>,
    pub resources: ResourcesView,
    pub events: Vec<Event>,
    pub trace: TraceView,
    /// The trace header's `Current Run: X`, underlined accent; empty renders
    /// no claim.
    pub current_run: String,
    /// The input bar's current text; empty shows the placeholder.
    pub command: String,
    /// Which page of the family is showing. The sub-pages live inside the
    /// harness occupant (memory's `PageMode` rule): the shell mounts one
    /// screen, this enum decides which member renders.
    pub mode: PageMode,
    /// The mounted Inspect page, present only while `mode` is
    /// [`PageMode::Inspect`]. Built by the shell from
    /// `harness_state::detail`; `None` there renders the honest
    /// [`super::inspect::EMPTY_PAGE`].
    pub inspect: Option<super::inspect::InspectView>,
    /// The full id the mounted Inspect page describes — what a refresh
    /// re-reads.
    pub inspect_id: Option<String>,
    /// The mounted Run Graph page, present only while `mode` is
    /// [`PageMode::Graph`]. Built by the shell from `harness_state::trace`
    /// for one run; a repo with no runs still mounts the page's documented
    /// no-run empty state, which is the whole of what it can honestly show.
    pub graph: Option<super::rungraph::GraphView>,
    /// The full id the mounted Run Graph draws: what a refresh re-reads.
    pub graph_id: Option<String>,
    /// ALL RUNS: selection index into [`Self::runs`] (the whole list, not
    /// the dashboard's visible three).
    pub all_selected: usize,
    /// A one-line notice, rendered above the input bar until dismissed.
    /// The honesty channel: keys whose backend does not exist say so here,
    /// and the ones that do report what they did.
    pub notice: Option<String>,
    /// Which card the plain arrows belong to. Two cards on this page have a
    /// selection and only one set of arrows, so the page says which is
    /// listening rather than guessing.
    pub focus: Focus,
    /// A destructive key waiting for its second press. Settings Reset's
    /// pattern: the first press arms and says what will happen, the second
    /// does it, and any other key disarms.
    pub armed: Option<Armed>,
    /// The session's posture word — [`crate::approval::current_mode_label`],
    /// empty when the caller published none.
    ///
    /// **The gates card needs it because a gate is not a posture.**
    /// `approval::current_gate()` resolves plan mode to `Gate::Ask`, since
    /// plan refuses before any gate is consulted, so a card drawing the gate
    /// alone says `Auto-approve: ASK` — "anything that writes asks you first"
    /// — over a session where nothing is asked and everything that writes is
    /// refused outright. The card names whichever of the two is actually
    /// deciding; see [`GATE_PLAN`].
    pub mode_label: String,
}

/// Which card the arrow keys are steering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Focus {
    #[default]
    Runs,
    Queue,
}

/// A destructive action that has been asked for once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Armed {
    /// Delete the session log of this run key.
    Delete(String),
    /// Remove every completed task from the task file.
    ClearDone,
}

/// The subtitle under the title, verbatim from the mock.
pub const SUBTITLE: &str = "Local Rust orchestration, runtime control, and agent execution";

/// The key that leaves this page, drawn on it — the chord that opened it.
/// Esc on this page dismisses a notice; it does not close the dashboard.
pub const EXIT_HINT: &str = "Alt+h closes";

/// The action bar's pairs, verbatim from the mock, in the mock's order.
pub const ACTIONS: [(&str, &str); 9] = [
    ("a", "Run"),
    ("p", "Pause"),
    ("r", "Resume"),
    ("x", "Cancel"),
    ("i", "Inspect"),
    ("l", "Logs"),
    ("g", "Graph"),
    ("R", "Refresh"),
    ("?", "Help"),
];

/// The honest empty states, one dim line inside the exact card chrome.
pub const EMPTY_RUNS: &str = "No runs yet — [a] starts one";
pub const EMPTY_WORKERS: &str = "No workers — single-process mode";
pub const EMPTY_QUEUE: &str = "Queue empty";
pub const EMPTY_GATES: &str = "Nothing pending";
pub const EMPTY_EVENTS: &str = "No events yet";

/// The input bar's placeholder, verbatim from the mock.
pub const PLACEHOLDER: &str = "Command palette (run, inspect, metrics, clear, ...)";

/// The dashboard shows at most this many run boxes — the mock's own count.
/// More runs exist behind the header's `View all →` (the ALL RUNS sub-page).
pub const VISIBLE_RUNS: usize = 3;

/// The ALL RUNS sub-page's action bar: only the keys that act there.
pub const ALL_RUNS_ACTIONS: [(&str, &str); 4] = [
    ("i", "Inspect"),
    ("b", "Back"),
    ("R", "Refresh"),
    ("?", "Help"),
];

/// The notices. Every key on this page answers with one of these: what it
/// did, or exactly why it could not.
///
/// `[a]`/`[+]` with an empty command bar. The bar is the goal, so there is
/// nothing to run yet and saying so beats starting an empty run.
pub const NOTICE_NO_GOAL: &str = "type a goal in the command bar, then [a] runs it here";
/// `[n]` with an empty command bar. One bar serves both writers.
pub const NOTICE_NO_TASK_TEXT: &str = "type the task in the command bar, then [n] adds it";
/// `[w]`. The one control on this page whose backend genuinely does not
/// exist, and the notice names the fact rather than the plan.
pub const NOTICE_WORKERS: &str = "no worker pool exists: delegation is a single permit in \
                                  delegate.rs, so there is nothing to scale";
/// `[D]`, armed.
pub const CONFIRM_DELETE: &str =
    "[D] again deletes that run's session log for good; any other key cancels";
/// `[c]`, armed.
pub const CONFIRM_CLEAR: &str =
    "[c] again drops every completed task from .emma/tasks/tasks.md; any other key cancels";
/// The queue keys, with nothing selected.
pub const NOTICE_NO_TASK: &str = "No task selected: Tab focuses the queue, ↑/↓ picks a row";
pub const NOTICE_LOGS: &str = "no logs view yet — the EVENT LOG card is the tail (plan H4)";
pub const NOTICE_NO_RUN: &str = "No run selected — ↑/↓ selects one";
/// The session history could not be read at all.
///
/// The memory page's `NOTICE_UNREADABLE`, arriving from this side: an
/// unreadable session directory and a directory with no runs in it produce the
/// same empty dashboard, and they mean opposite things. It names *this* store,
/// so a reader who sees one of the two sentences knows which one failed.
///
/// ⚠ **Untested from `guarantees.rs`.** The session directory is a private
/// field with no seam a sibling module can aim at a tempdir, so nothing in the
/// hardening net drives this arm — see that file's F32 region.
pub const NOTICE_UNREADABLE: &str =
    "the session history could not be read — not the same as no runs";
/// The `[?]` help notices, one per key scope, naming only keys that work.
///
/// **`HELP_DASH` has two spellings and the split is per platform, not per
/// taste.** The three run controls are `runctl` signals and Windows has none
/// of them ([`crate::runctl::supported`]), so a single string would either
/// advertise three keys that answer with a refusal there, or hide three keys
/// that work everywhere else. This is the `cfg` ruling 4 asks for — the one
/// that makes the sentence true on each platform — and not the one it
/// forbids, which is a predicate answering `true` where the platform answers
/// nothing. A test asserts the text names pause exactly when the platform
/// can pause, so the two cannot drift apart.
#[cfg(unix)]
pub const HELP_DASH: &str = "[a] run  [p]/[r]/[x] pause/resume/cancel  [A]rchive [D]elete  \
[n]ew task [c]lear done  [t]/[s]/[m]/[d] next-run policy  [i]nspect [g]raph [v]iew all  \
Tab switches which card ↑/↓ steers";
#[cfg(not(unix))]
pub const HELP_DASH: &str = "[a] run  [A]rchive [D]elete  [n]ew task [c]lear done  \
[t]/[s]/[m]/[d] next-run policy  [i]nspect [g]raph [v]iew all  \
Tab switches which card ↑/↓ steers  (pause/resume/cancel: press one for why)";
pub const HELP_ALL: &str = "[i]/[Enter] inspect  [b] back  [R] refresh  ↑/↓ select";
pub const HELP_INSPECT: &str = "[b] back  ↑/↓ select a tool call  [R] refresh";
pub const HELP_GRAPH: &str =
    "[b] back  ↑/↓ select a node  j/k pan  PgUp/PgDn page  Home/End  [:] command bar  [R] refresh";

/// What the TOOL GATES header says while the session is in plan mode.
///
/// **A gate and a posture are different questions and the card was answering
/// the wrong one.** `approval::current_gate()` resolves plan to `Gate::Ask`,
/// correctly — plan is not a gate, it refuses before a gate is reached — so
/// the card drew `Auto-approve: ASK`, whose own sentence is "anything that
/// writes or leaves the machine asks you first". In plan mode nothing asks:
/// it is refused with the mode named, and the reader waiting for a prompt
/// waits forever. The plan-mode package (S2) left this open; the card now
/// names whichever of the two is deciding.
pub const GATE_PLAN: &str = "PLAN (refused, not asked)";

/// The three action-bar letters that are [`crate::runctl`] controls, paired
/// with what they ask for, so the bar can ask whether this platform has them
/// before it draws them.
const CONTROL_KEYS: [(&str, crate::runctl::Action); 3] = [
    ("p", crate::runctl::Action::Pause),
    ("r", crate::runctl::Action::Resume),
    ("x", crate::runctl::Action::Cancel),
];

/// The sentence the action bar carries when this platform cannot carry out
/// the run controls, or `None` when it can.
///
/// **It names the platform, because "unavailable" without a reason reads as a
/// bug.** `std::env::consts::OS` rather than a `cfg`-selected literal: the
/// word and the answer then come from the same build, and a third platform
/// gets a true sentence without an edit here. The chip itself goes dim and
/// the key still answers with [`crate::runctl::Refusal::Unsupported`]'s own
/// wording, which is the part that says *why*; this row exists so a reader
/// does not have to press a key to find out that one is not on offer.
pub fn no_controls_note() -> Option<String> {
    let missing: Vec<&(&str, crate::runctl::Action)> = CONTROL_KEYS
        .iter()
        .filter(|(_, a)| !crate::runctl::supported(*a))
        .collect();
    if missing.is_empty() {
        return None;
    }
    let keys: Vec<&str> = missing.iter().map(|(k, _)| *k).collect();
    let names: Vec<&str> = missing.iter().map(|(_, a)| a.wire()).collect();
    Some(format!(
        // A hyphen, not an em dash: this string is built without a
        // `Skin` and the page has an ASCII skin whose whole rule is that no
        // multibyte glyph reaches the buffer.
        "[{}] {} are not offered on {} - press one for the reason",
        keys.join("/"),
        names.join("/"),
        std::env::consts::OS,
    ))
}

/// Whether the action bar's pair at `key` is a control this platform lacks.
fn control_unavailable(key: &str) -> bool {
    CONTROL_KEYS
        .iter()
        .any(|(k, a)| *k == key && !crate::runctl::supported(*a))
}

// endregion: State

// region: Keys
// ---------------------------------------------------------------------------
// Keys — the pure seam (memory's M5 pattern, applied to the harness family)
// ---------------------------------------------------------------------------

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

/// Which page of the harness family is showing. The family lives inside the
/// one harness occupant — `App` never grows a second screen for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PageMode {
    #[default]
    Dashboard,
    /// Every run the session logs record — the `View all →` door.
    AllRuns,
    /// `term/inspect.rs`, mounted on one run.
    Inspect,
    /// `term/rungraph.rs`, mounted with its honest no-run state (HB4).
    Graph,
}

/// What a key asks the shell to do. Everything that needs disk crosses this
/// enum; [`handle_key`] never touches a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HarnessAction {
    /// Not this page's key (or a release/chord): let it fall through.
    None,
    /// The view changed (selection, mode, notice) and wants a repaint.
    FocusChanged,
    /// Re-read the session history and rebuild the view.
    Refresh,
    /// Mount the Inspect page for this full run id ([`Run::key`]).
    Inspect(String),
    /// Mount the Run Graph for this full run id ([`Run::key`]).
    Graph(String),
    /// `[?]` fired; the view's notice was already toggled.
    Help,
    /// Signal one run's process: pause, resume or cancel, by full run id.
    /// The verification and the signal are the shell's, through
    /// [`crate::runctl`]; this enum only carries the ask.
    ///
    /// **Never emitted where [`crate::runctl::supported`] is `false`.** The
    /// key answers with the refusal's own sentence instead, so a platform
    /// with no such control has no path from a keystroke to a process.
    Signal(crate::runctl::Action, String),
    /// Start a new run: this goal, headless, in the page's repository.
    Launch(String),
    /// Move one run's session log into the archive subdirectory.
    Archive(String),
    /// Delete one run's session log. Only ever emitted by the second press.
    Delete(String),
    /// Add a task with this text to `.emma/tasks/tasks.md`.
    TaskNew(String),
    /// Drop every completed task. Only ever emitted by the second press.
    TaskClearDone,
    /// Move the selected task one place up (`true`) or down.
    TaskMove(bool),
    /// Write a next-run tool policy to the repository's `settings.local.json`.
    Policy(crate::runctl::Policy),
    /// Report what that file says today, writing nothing.
    PolicyShow,
    /// Esc with nothing left to dismiss: the page itself should close, the
    /// way Esc closes Settings. Without this the only way off the screen is
    /// the Alt+h chord that opened it, which a reader who arrived by clicking
    /// has no reason to know.
    Close,
}

/// One key, against the open harness page. Alt/Ctrl chords and releases are
/// never this page's: they return [`HarnessAction::None`] untouched so the
/// global layer (Alt+h, Ctrl-C…) keeps working over an open page.
pub fn handle_key(v: &mut HarnessView, key: KeyEvent) -> HarnessAction {
    if key.kind == KeyEventKind::Release
        || key
            .modifiers
            .intersects(KeyModifiers::ALT | KeyModifiers::CONTROL)
    {
        return HarnessAction::None;
    }
    match v.mode {
        PageMode::Dashboard => dashboard_key(v, key),
        PageMode::AllRuns => all_runs_key(v, key),
        PageMode::Inspect => inspect_key(v, key),
        PageMode::Graph => graph_key(v, key),
    }
}

/// How many run boxes the dashboard is showing — what the cycle wraps within.
fn visible_runs(v: &HarnessView) -> usize {
    v.runs.len().min(VISIBLE_RUNS)
}

/// Move the dashboard selection by one box, wrapping within the visible.
fn cycle(v: &mut HarnessView, forward: bool) {
    let n = visible_runs(v);
    if n == 0 {
        return;
    }
    let cur = v.selected_run.unwrap_or(0).min(n - 1);
    v.selected_run = Some(if forward {
        (cur + 1) % n
    } else {
        (cur + n - 1) % n
    });
}

/// `[g]` on a run: the graph is drawn from that run's records, so the shell
/// has to read them. A repo with no run has nothing to read and mounts the
/// page's own no-run state here, which is the one case this page can answer
/// without touching disk.
fn graph_selected(v: &mut HarnessView, sel: Option<usize>) -> HarnessAction {
    // The selected run, else the newest in this repo: `[g]` from a page where
    // nothing is selected still means "graph what just ran".
    match sel.or(Some(0)).and_then(|i| v.runs.get(i)) {
        Some(run) => HarnessAction::Graph(run.key.clone()),
        None => {
            v.graph = Some(super::rungraph::GraphView {
                version: v.version.clone(),
                ..super::rungraph::GraphView::default()
            });
            v.graph_id = None;
            v.mode = PageMode::Graph;
            v.notice = None;
            HarnessAction::FocusChanged
        }
    }
}

/// `[i]`/Enter: the selected run's full id, or the honest ask for one.
fn inspect_selected(v: &mut HarnessView, index: Option<usize>) -> HarnessAction {
    match index.and_then(|i| v.runs.get(i)) {
        Some(run) => {
            v.notice = None;
            HarnessAction::Inspect(run.key.clone())
        }
        None => {
            v.notice = Some(NOTICE_NO_RUN.to_string());
            HarnessAction::FocusChanged
        }
    }
}

/// `[?]`: the scope's help line, toggled off when it is already up.
fn toggle_help(v: &mut HarnessView, text: &str) {
    if v.notice.as_deref() == Some(text) {
        v.notice = None;
    } else {
        v.notice = Some(text.to_string());
    }
}

/// The run the dashboard's run keys act on: the selected box, else the
/// newest. `[p]` from a page where nothing is selected still means "pause
/// what is running", which is the reading `[g]` already takes.
fn acting_run(v: &HarnessView) -> Option<&Run> {
    v.selected_run
        .filter(|&i| i < visible_runs(v))
        .or(Some(0))
        .and_then(|i| v.runs.get(i))
}

/// One run key, or the honest ask for a run when there is none.
fn on_run(v: &mut HarnessView, make: impl FnOnce(&Run) -> HarnessAction) -> HarnessAction {
    match acting_run(v) {
        Some(run) => {
            let action = make(run);
            v.notice = None;
            action
        }
        None => {
            v.notice = Some(NOTICE_NO_RUN.to_string());
            HarnessAction::FocusChanged
        }
    }
}

/// One of the three run controls, or the platform's reason for not having it.
///
/// **The platform question is asked before the selection question**, and the
/// order is deliberate: "no run is selected" would be a true sentence and the
/// wrong one, because selecting a run would not make the key work. Ruling 4's
/// shape — the key stays bound, the answer is words, and no
/// [`HarnessAction::Signal`] is ever produced, so nothing downstream can
/// mistake this for a signal that went.
fn control_key(v: &mut HarnessView, action: crate::runctl::Action) -> HarnessAction {
    if !crate::runctl::supported(action) {
        // The refusal writes its own sentence, naming the platform and what it
        // cannot do; re-wording it here would be a second copy to keep true.
        v.notice = Some(crate::runctl::Refusal::Unsupported(action).to_string());
        return HarnessAction::FocusChanged;
    }
    on_run(v, |r| HarnessAction::Signal(action, r.key.clone()))
}

/// Move the TASK QUEUE selection by one row.
fn cycle_task(v: &mut HarnessView, forward: bool) {
    let n = v.tasks.len();
    if n == 0 {
        return;
    }
    let cur = v.selected_task.unwrap_or(0).min(n - 1);
    v.selected_task = Some(if forward {
        (cur + 1) % n
    } else {
        (cur + n - 1) % n
    });
}

/// The dashboard's keys. Every advertised letter answers: with the real
/// action where a backend exists, with the truth where none does.
///
/// **The two-press keys are checked first.** `[D]` and `[c]` arm on their
/// first press and act on their second, and every other key disarms, so a
/// destructive action cannot be reached by one keystroke and cannot be left
/// armed behind the reader's back (Settings Reset's rule).
fn dashboard_key(v: &mut HarnessView, key: KeyEvent) -> HarnessAction {
    use crate::runctl::{Action, Policy};

    if let Some(armed) = v.armed.take() {
        match (&armed, key.code) {
            (Armed::Delete(id), KeyCode::Char('D')) => {
                let id = id.clone();
                v.notice = None;
                return HarnessAction::Delete(id);
            }
            (Armed::ClearDone, KeyCode::Char('c')) => {
                v.notice = None;
                return HarnessAction::TaskClearDone;
            }
            // Anything else cancels, and the key it was is not also acted on:
            // the press that cancels a confirmation belongs to the
            // confirmation.
            _ => {
                v.notice = None;
                return HarnessAction::FocusChanged;
            }
        }
    }

    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    match key.code {
        // Shifted arrows reorder the queue; plain ones steer whichever card
        // has focus. Terminals that cannot report shift on an arrow simply do
        // not reorder, which is why the footer names the chord.
        KeyCode::Up | KeyCode::Down if shift && v.focus == Focus::Queue => {
            if v.tasks.is_empty() {
                v.notice = Some(NOTICE_NO_TASK.to_string());
                return HarnessAction::FocusChanged;
            }
            HarnessAction::TaskMove(key.code == KeyCode::Up)
        }
        KeyCode::Down => {
            match v.focus {
                Focus::Runs => cycle(v, true),
                Focus::Queue => cycle_task(v, true),
            }
            HarnessAction::FocusChanged
        }
        KeyCode::Up | KeyCode::BackTab => {
            match v.focus {
                Focus::Runs => cycle(v, false),
                Focus::Queue => cycle_task(v, false),
            }
            HarnessAction::FocusChanged
        }
        // Tab moves the arrows between the two cards that have a selection.
        // It used to duplicate Down, which was a key spent on nothing.
        KeyCode::Tab => {
            v.focus = match v.focus {
                Focus::Runs => Focus::Queue,
                Focus::Queue => Focus::Runs,
            };
            if v.focus == Focus::Queue && v.selected_task.is_none() && !v.tasks.is_empty() {
                v.selected_task = Some(0);
            }
            HarnessAction::FocusChanged
        }
        KeyCode::Enter | KeyCode::Char('i') => {
            let sel = v.selected_run.filter(|&i| i < visible_runs(v));
            inspect_selected(v, sel)
        }
        KeyCode::Char('g') => graph_selected(v, v.selected_run.filter(|&i| i < visible_runs(v))),
        KeyCode::Char('v') => {
            v.mode = PageMode::AllRuns;
            v.all_selected = v
                .selected_run
                .unwrap_or(0)
                .min(v.runs.len().saturating_sub(1));
            v.notice = None;
            HarnessAction::FocusChanged
        }
        KeyCode::Char('l') => {
            v.notice = Some(NOTICE_LOGS.to_string());
            HarnessAction::FocusChanged
        }
        KeyCode::Char('R') => HarnessAction::Refresh,
        // The three signals. The shell verifies the process before sending
        // one; this half only names the run, and only where the platform has
        // the control at all.
        KeyCode::Char('p') => control_key(v, Action::Pause),
        KeyCode::Char('r') => control_key(v, Action::Resume),
        KeyCode::Char('x') => control_key(v, Action::Cancel),
        // The command bar is the goal. An empty bar is not a run.
        KeyCode::Char('a') | KeyCode::Char('+') => {
            let goal = v.command.trim().to_string();
            if goal.is_empty() {
                v.notice = Some(NOTICE_NO_GOAL.to_string());
                return HarnessAction::FocusChanged;
            }
            HarnessAction::Launch(goal)
        }
        KeyCode::Char('A') => on_run(v, |r| HarnessAction::Archive(r.key.clone())),
        KeyCode::Char('D') => match acting_run(v) {
            Some(run) => {
                let id = run.key.clone();
                v.armed = Some(Armed::Delete(id));
                v.notice = Some(CONFIRM_DELETE.to_string());
                HarnessAction::FocusChanged
            }
            None => {
                v.notice = Some(NOTICE_NO_RUN.to_string());
                HarnessAction::FocusChanged
            }
        },
        // The queue. `[n]` takes the command bar the same way `[a]` does, so
        // one input bar serves both writers and there is nowhere else to type.
        KeyCode::Char('n') => {
            let text = v.command.trim().to_string();
            if text.is_empty() {
                v.notice = Some(NOTICE_NO_TASK_TEXT.to_string());
                return HarnessAction::FocusChanged;
            }
            HarnessAction::TaskNew(text)
        }
        KeyCode::Char('c') => {
            v.armed = Some(Armed::ClearDone);
            v.notice = Some(CONFIRM_CLEAR.to_string());
            HarnessAction::FocusChanged
        }
        // The next-run tool policy. `[t]` reports what the file says today
        // and writes nothing; the other three are the setting.
        KeyCode::Char('t') => HarnessAction::PolicyShow,
        KeyCode::Char('s') => HarnessAction::Policy(Policy::Safe),
        KeyCode::Char('m') => HarnessAction::Policy(Policy::Manual),
        KeyCode::Char('d') => HarnessAction::Policy(Policy::DenyAll),
        KeyCode::Char('w') => {
            v.notice = Some(NOTICE_WORKERS.to_string());
            HarnessAction::FocusChanged
        }
        KeyCode::Char('?') => {
            toggle_help(v, HELP_DASH);
            HarnessAction::Help
        }
        KeyCode::Esc => {
            if v.notice.take().is_some() {
                HarnessAction::FocusChanged
            } else {
                HarnessAction::Close
            }
        }
        _ => HarnessAction::None,
    }
}

/// The ALL RUNS sub-page's keys: the whole history is reachable here.
fn all_runs_key(v: &mut HarnessView, key: KeyEvent) -> HarnessAction {
    match key.code {
        KeyCode::Up => {
            v.all_selected = v.all_selected.saturating_sub(1);
            HarnessAction::FocusChanged
        }
        KeyCode::Down => {
            v.all_selected = (v.all_selected + 1).min(v.runs.len().saturating_sub(1));
            HarnessAction::FocusChanged
        }
        KeyCode::Enter | KeyCode::Char('i') => {
            let sel = (!v.runs.is_empty()).then_some(v.all_selected);
            inspect_selected(v, sel)
        }
        KeyCode::Char('b') | KeyCode::Esc => {
            v.mode = PageMode::Dashboard;
            v.notice = None;
            HarnessAction::FocusChanged
        }
        KeyCode::Char('R') => HarnessAction::Refresh,
        KeyCode::Char('?') => {
            toggle_help(v, HELP_ALL);
            HarnessAction::Help
        }
        _ => HarnessAction::None,
    }
}

/// The Inspect page's keys: back out, cycle the tool-call selection (the
/// list the logs actually back), refresh.
fn inspect_key(v: &mut HarnessView, key: KeyEvent) -> HarnessAction {
    match key.code {
        KeyCode::Char('b') | KeyCode::Esc => {
            v.mode = PageMode::Dashboard;
            v.inspect = None;
            v.inspect_id = None;
            v.notice = None;
            HarnessAction::FocusChanged
        }
        KeyCode::Up | KeyCode::Down => {
            if let Some(run) = v.inspect.as_mut().and_then(|iv| iv.run.as_mut()) {
                let n = run.tools.len();
                if n > 0 {
                    let cur = run.selected_tool.unwrap_or(0).min(n - 1);
                    run.selected_tool = Some(if key.code == KeyCode::Down {
                        (cur + 1) % n
                    } else {
                        (cur + n - 1) % n
                    });
                }
            }
            HarnessAction::FocusChanged
        }
        KeyCode::Char('R') => HarnessAction::Refresh,
        KeyCode::Char('?') => {
            toggle_help(v, HELP_INSPECT);
            HarnessAction::Help
        }
        _ => HarnessAction::None,
    }
}

/// The Run Graph page's keys: back, help, the selection the DAG footer
/// promises, and the command bar.
///
/// **The bar is a mode, not a default.** Every page key here is a letter, so
/// a page that sent every letter to the bar could not be left with `b`, and
/// one that sent none could never type `bash`. `:` and `/` hand it the
/// keyboard and `Esc` takes it back, which is the only arrangement in which
/// both keys mean one thing.
fn graph_key(v: &mut HarnessView, key: KeyEvent) -> HarnessAction {
    use super::rungraph as rg;
    if v.graph.as_ref().is_some_and(|gv| gv.palette) {
        let Some(gv) = v.graph.as_mut() else {
            return HarnessAction::None;
        };
        return match key.code {
            KeyCode::Char(c) => {
                gv.query.push(c);
                gv.notice = None;
                HarnessAction::FocusChanged
            }
            KeyCode::Backspace => {
                gv.query.pop();
                HarnessAction::FocusChanged
            }
            KeyCode::Enter => {
                rg::command(gv);
                gv.palette = false;
                HarnessAction::FocusChanged
            }
            KeyCode::Esc => {
                gv.query.clear();
                gv.palette = false;
                gv.notice = None;
                HarnessAction::FocusChanged
            }
            _ => HarnessAction::FocusChanged,
        };
    }
    match key.code {
        KeyCode::Char(':') | KeyCode::Char('/') => {
            if let Some(gv) = v.graph.as_mut() {
                gv.palette = true;
                gv.notice = None;
            }
            HarnessAction::FocusChanged
        }
        // The selection leads and the canvas follows: `navigate` reveals
        // what it selected, so a node below the fold is scrolled to rather
        // than silently inspected off screen.
        KeyCode::Up | KeyCode::Down => {
            if let Some(gv) = v.graph.as_mut() {
                rg::navigate(gv, key.code == KeyCode::Down);
            }
            HarnessAction::FocusChanged
        }
        // The explicit pan, for reading the graph without moving the
        // selection. Vertical only, because the layout centers every layer
        // inside the panel and there is nothing off to the side to reach.
        KeyCode::Char('j') | KeyCode::Char('k') => {
            if let Some(gv) = v.graph.as_mut() {
                rg::pan(gv, key.code == KeyCode::Char('j'), 1);
            }
            HarnessAction::FocusChanged
        }
        KeyCode::PageDown | KeyCode::PageUp => {
            if let Some(gv) = v.graph.as_mut() {
                let page = gv.canvas_rows.max(1);
                rg::pan(gv, key.code == KeyCode::PageDown, page);
            }
            HarnessAction::FocusChanged
        }
        KeyCode::Home | KeyCode::End => {
            if let Some(gv) = v.graph.as_mut() {
                rg::pan_to(gv, key.code == KeyCode::Home);
            }
            HarnessAction::FocusChanged
        }
        KeyCode::Char('R') => HarnessAction::Refresh,
        KeyCode::Char('b') | KeyCode::Esc => {
            v.mode = PageMode::Dashboard;
            v.graph = None;
            v.graph_id = None;
            v.notice = None;
            HarnessAction::FocusChanged
        }
        KeyCode::Char('?') => {
            toggle_help(v, HELP_GRAPH);
            HarnessAction::Help
        }
        _ => HarnessAction::None,
    }
}

// endregion: Keys

// region: Mouse
// ---------------------------------------------------------------------------
// Mouse — rects recorded at paint, a pure hit-test after (the sessions-[+]
// precedent: `app::sidebar_click` / `sessions_add`)
// ---------------------------------------------------------------------------

/// Where the dashboard's controls were on the last paint. Recorded by
/// [`render_hits`] from the same arithmetic that painted them, so the two
/// cannot drift; empty on the sub-pages, whose controls are keys today.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Hits {
    /// One rect per painted action chip, with the key it fires.
    pub chips: Vec<(Rect, char)>,
    /// One rect per painted ACTIVE RUNS mini-box, in run order.
    pub runs: Vec<Rect>,
    /// How many rows the Run Graph's DAG canvas got on the last paint. Zero
    /// on every other page. The viewport is the one thing a pure key handler
    /// cannot measure, so the paint reports it the way the rects do.
    pub graph_canvas_rows: u16,
}

/// What a left-button press means, given where the controls were painted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hit {
    /// A run box: select it.
    Run(usize),
    /// An action chip: fire the same action as its key.
    Chip(char),
}

/// The pure hit-test. `None` for every other cell — a page where one control
/// works and the rest swallows clicks is worse than one the pointer passes
/// through (the sidebar's rule).
pub fn hit(hits: &Hits, col: u16, row: u16) -> Option<Hit> {
    let inside = |r: &Rect| col >= r.x && col < r.x + r.width && row >= r.y && row < r.y + r.height;
    if let Some((_, key)) = hits.chips.iter().find(|(r, _)| inside(r)) {
        return Some(Hit::Chip(*key));
    }
    hits.runs.iter().position(inside).map(Hit::Run)
}

// endregion: Mouse

// region: Rendering
// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

/// The page's own glyphs, one ASCII fallback each — the `render::ASCII`
/// split, extended for shapes that file does not carry.
struct PageGlyphs {
    play: &'static str,
    dot: &'static str,
    robot: &'static str,
    check: &'static str,
    cross: &'static str,
    hourglass: &'static str,
    arrow: &'static str,
    full: &'static str,
    empty: &'static str,
    mid: &'static str,
    endash: &'static str,
    dash: &'static str,
    updown: &'static str,
    /// The shift prefix on a chord's arrows.
    shift: &'static str,
}

fn page_glyphs(skin: &Skin) -> PageGlyphs {
    if skin.glyphs == ASCII {
        PageGlyphs {
            play: ">",
            dot: "*",
            robot: "@",
            check: "+",
            cross: "x",
            hourglass: "~",
            arrow: "->",
            full: "#",
            empty: "-",
            mid: ".",
            endash: "-",
            dash: "-",
            updown: "^/v",
            shift: "S-",
        }
    } else {
        PageGlyphs {
            play: "▶",
            dot: "●",
            robot: "🤖",
            check: "✓",
            cross: "✗",
            hourglass: "⏳",
            arrow: "→",
            full: "█",
            empty: "░",
            mid: "·",
            endash: "–",
            dash: "—",
            updown: "↑/↓",
            shift: "⇧",
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

/// Draw whichever member of the family [`HarnessView::mode`] names.
pub fn render(area: Rect, buf: &mut Buffer, v: &HarnessView, skin: &Skin) {
    let _ = render_hits(area, buf, v, skin);
}

/// [`render`], reporting where the clickable controls landed. The shell
/// stores the result and hit-tests presses against it.
pub fn render_hits(area: Rect, buf: &mut Buffer, v: &HarnessView, skin: &Skin) -> Hits {
    if area.width < 4 || area.height < 2 {
        return Hits::default();
    }
    let mut hits = Hits::default();
    match v.mode {
        PageMode::Dashboard => return render_dashboard(area, buf, v, skin),
        PageMode::AllRuns => render_all_runs(area, buf, v, skin),
        // A mounted view or the family's honest fallback: `run: None` is the
        // Inspect page's own no-run state, an empty GraphView the graph's.
        PageMode::Inspect => match &v.inspect {
            Some(iv) => super::inspect::render(area, buf, iv, skin),
            None => super::inspect::render(
                area,
                buf,
                &super::inspect::InspectView {
                    version: v.version.clone(),
                    run: None,
                },
                skin,
            ),
        },
        PageMode::Graph => {
            hits.graph_canvas_rows = match &v.graph {
                Some(gv) => super::rungraph::render(area, buf, gv, skin),
                None => super::rungraph::render(
                    area,
                    buf,
                    &super::rungraph::GraphView {
                        version: v.version.clone(),
                        ..super::rungraph::GraphView::default()
                    },
                    skin,
                ),
            }
        }
    }
    hits
}

/// The ALL RUNS sub-page: the main page's head idiom, its own action bar,
/// one full-width listing card, and the bottom-anchored notice/hint row —
/// memory's derived sub-view skeleton, applied (owner directive 2026-08-26:
/// `View all →` is the door to the whole history).
fn render_all_runs(area: Rect, buf: &mut Buffer, v: &HarnessView, skin: &Skin) {
    let g = page_glyphs(skin);
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
            fit("All Runs", w, skin.glyphs.ellipsis),
            skin.palette.bold(Role::Accent),
        )),
        Line::from(Span::styled(
            fit(
                "Every run this repo's session logs record",
                w,
                skin.glyphs.ellipsis,
            ),
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
    buf.set_line(
        content.x,
        y,
        &keybar(&ALL_RUNS_ACTIONS, "   ", skin),
        content.width,
    );
    y += 2;
    if y < bottom {
        let sel = v.all_selected.min(v.runs.len().saturating_sub(1));
        let right = if v.runs.is_empty() {
            String::new()
        } else {
            format!("{} of {}", sel + 1, v.runs.len())
        };
        let card = card_frame(
            Rect::new(content.x, y, content.width, bottom - y),
            buf,
            skin,
            &format!("ALL RUNS ({})", v.runs.len()),
            vec![Span::styled(right, skin.palette.dim())],
        );
        let cw = usize::from(card.width);
        if v.runs.is_empty() {
            set_row(card, 0, buf, empty_line(EMPTY_RUNS, cw, skin));
        }
        for (row_i, i) in window(v.runs.len(), sel, usize::from(card.height)).enumerate() {
            let run = &v.runs[i];
            let (word, style) = match run.state {
                RunState::Running => ("Running", skin.palette.style(Role::Ok)),
                RunState::Paused => ("Paused", skin.palette.style(Role::Warn)),
                RunState::Completed => ("Completed", skin.palette.dim()),
                RunState::Failed => ("Failed", skin.palette.style(Role::Err)),
            };
            let pct = if run.total == 0 {
                String::new()
            } else {
                let p = u64::from(run.done) * 100 / u64::from(run.total);
                format!("  {}/{} ({p}%)", run.done, run.total)
            };
            let mut trailer = String::new();
            if !run.id.is_empty() {
                trailer.push_str(&format!("{} {} ", run.id, g.mid));
            }
            trailer.push_str(&run.stamp);
            let mut line = lr(
                vec![
                    Span::styled(format!("{} ", g.play), skin.palette.style(Role::Accent)),
                    Span::styled(run.name.clone(), skin.palette.style(Role::Text)),
                    Span::styled(format!("  {trailer}"), skin.palette.dim()),
                ],
                vec![
                    Span::styled(word.to_string(), style),
                    Span::styled(pct, skin.palette.dim()),
                ],
                cw,
                skin,
            );
            if i == sel {
                line = banded(line, skin.palette.chip(Role::Accent));
            }
            set_row(card, row_i as u16, buf, line);
        }
    }
    if let Some(h) = hint_area {
        let text = v
            .notice
            .clone()
            .unwrap_or_else(|| "[b] back to Harness".to_string());
        let style = if v.notice.is_some() {
            skin.palette.style(Role::Accent)
        } else {
            skin.palette.dim()
        };
        let ascii = skin.glyphs == ASCII;
        let line = Line::from(Span::styled(
            fit(&honest(&text, ascii), w, skin.glyphs.ellipsis),
            style,
        ));
        buf.set_line(h.x, h.y, &line, h.width);
    }
}

/// A selection-following window: the selected row stays visible and rows
/// above scroll away rather than truncate — memory's listing rule.
fn window(len: usize, selected: usize, viewport: usize) -> std::ops::Range<usize> {
    if viewport == 0 || len == 0 {
        return 0..0;
    }
    let top = (selected + 1)
        .saturating_sub(viewport)
        .min(len.saturating_sub(viewport));
    top..(top + viewport).min(len)
}

/// The dashboard: the mock, cell for cell — with the input bar (and the
/// notice row, when one is up) **bottom-anchored** like memory's query box:
/// the owner's short-terminal defect class, where a bar rendered last,
/// top-down, fell off the screen. The grid takes what is left and
/// compresses honestly (see [`band_heights`]).
fn render_dashboard(area: Rect, buf: &mut Buffer, v: &HarnessView, skin: &Skin) -> Hits {
    let g = page_glyphs(skin);
    let notice_h = u16::from(v.notice.is_some());
    let (content, notice_area, input_area) = if area.height >= 7 + notice_h {
        let [c, n, q] = Layout::vertical([
            Constraint::Fill(1),
            Constraint::Length(notice_h),
            Constraint::Length(3),
        ])
        .areas(area);
        (c, (notice_h > 0).then_some(n), Some(q))
    } else if area.height >= 3 {
        let [c, q] = Layout::vertical([Constraint::Fill(1), Constraint::Length(3)]).areas(area);
        (c, None, Some(q))
    } else {
        (area, None, None)
    };
    let hits = render_grid(content, buf, v, skin, &g);
    if let (Some(n), Some(text)) = (notice_area, &v.notice) {
        let line = Line::from(Span::styled(
            fit(
                &honest(text, skin.glyphs == ASCII),
                usize::from(n.width),
                skin.glyphs.ellipsis,
            ),
            skin.palette.style(Role::Accent),
        ));
        buf.set_line(n.x, n.y, &line, n.width);
    }
    if let Some(q) = input_area {
        render_input(q, buf, v, skin);
    }
    hits
}

/// Head, chip bar, and the card grid, into the space the anchor left over.
fn render_grid(area: Rect, buf: &mut Buffer, v: &HarnessView, skin: &Skin, g: &PageGlyphs) -> Hits {
    let mut hits = Hits::default();
    if area.width < 4 || area.height == 0 {
        return hits;
    }
    let w = usize::from(area.width);
    let bottom = area.y + area.height;
    let mut y = area.y;

    // The head: version in the corner, the title, the subtitle, a rule.
    // "Large" is not a thing a terminal cell can do; one bold accent row is
    // this repository's standing substitute (settings design Q8).
    let head = [
        corner_row(EXIT_HINT, &v.version, w, skin),
        Line::from(Span::styled(
            fit("Harness", w, skin.glyphs.ellipsis),
            skin.palette.bold(Role::Accent),
        )),
        // The subtitle row carries the platform's refusal, flush right: it is
        // the one full-width row with space to spare, it sits directly over
        // the chips it is about, and a reader who has not pressed anything is
        // the reader who needs it. Dropped rather than truncated when the
        // window cannot hold it — half a sentence about what does not work
        // is worse than the dim chip it was explaining.
        match no_controls_note().filter(|n| cols(n) + 4 <= w) {
            Some(note) => lr(
                vec![Span::styled(SUBTITLE.to_string(), skin.palette.dim())],
                vec![Span::styled(note, skin.palette.style(Role::Warn))],
                w,
                skin,
            ),
            None => Line::from(Span::styled(
                fit(SUBTITLE, w, skin.glyphs.ellipsis),
                skin.palette.dim(),
            )),
        },
        Line::from(Span::styled(skin.glyphs.rule.repeat(w), skin.palette.dim())),
    ];
    for line in head {
        if y >= bottom {
            return hits;
        }
        buf.set_line(area.x, y, &line, area.width);
        y += 1;
    }

    // The action bar as clickable chips (owner directive 2026-08-26): the
    // bracketed key inside a small colored box, the label dim beside it,
    // two columns of air — the mock's own spacing, the mock's own letters.
    if y >= bottom {
        return hits;
    }
    hits.chips = chip_bar(area.x, y, w, buf, skin);
    y += 2; // the bar, then one row of air before the grid

    // The grid columns, one column of air between them. The mock's top two
    // bands are three cards wide, not two: ACTIVE RUNS | WORKER POOL |
    // RUNTIME STATUS over TASK QUEUE | TOOL GATES | RESOURCES.
    let grid = Rect::new(area.x, y.min(bottom), area.width, bottom.saturating_sub(y));
    let [lc, _, mc, _, rc] = Layout::horizontal([
        Constraint::Fill(1),
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(1),
        Constraint::Fill(1),
    ])
    .areas(grid);

    // Each pair shares a band as tall as its taller card, the settings grid's
    // rule; footers sink to the card's last inner row. Desired first, then
    // [`band_heights`] compresses when the window is short.
    let shown_runs = v.runs.len().min(VISIBLE_RUNS);
    let runs_rows = if v.runs.is_empty() {
        1
    } else {
        shown_runs as u16 * 5 + u16::from(v.runs.len() > shown_runs)
    };
    let runs_h = runs_rows + 4;
    let pool_h = v.workers.len().max(1) as u16 + 4;
    let runtime_h = 13;
    let queue_h = v.tasks.len().max(1) as u16 + 5;
    let gates_h = v.gates.len().max(1) as u16 + 5;
    let resources_h = 13;
    let events_h = v.events.len().max(1) as u16 + 3;
    let trace_h = 10;
    let [h1, h2, h3] = band_heights(
        grid.height,
        [
            runs_h.max(pool_h).max(runtime_h),
            queue_h.max(gates_h).max(resources_h),
            events_h.max(trace_h),
        ],
    );

    if h1 >= 3 {
        hits.runs = render_runs(
            Rect::new(lc.x, y, lc.width, h1),
            buf,
            v,
            skin,
            g,
            &mut hits.chips,
        );
        render_pool(
            Rect::new(mc.x, y, mc.width, h1),
            buf,
            v,
            skin,
            g,
            &mut hits.chips,
        );
        render_runtime(Rect::new(rc.x, y, rc.width, h1), buf, v, skin, g);
        y += h1;
    }
    if h2 >= 3 {
        render_queue(
            Rect::new(lc.x, y, lc.width, h2),
            buf,
            v,
            skin,
            g,
            &mut hits.chips,
        );
        render_gates(
            Rect::new(mc.x, y, mc.width, h2),
            buf,
            v,
            skin,
            g,
            &mut hits.chips,
        );
        render_resources(Rect::new(rc.x, y, rc.width, h2), buf, v, skin, g);
        y += h2;
    }

    // The full-width band: EVENT LOG wide left, TRACE / CONTEXT right, the
    // mock's bottom row of two before the input bar.
    if h3 >= 3 {
        let band = Rect::new(area.x, y, area.width, h3);
        let [el, _, tr] = Layout::horizontal([
            Constraint::Fill(2),
            Constraint::Length(1),
            Constraint::Fill(1),
        ])
        .areas(band);
        render_events(el, buf, v, skin, g);
        render_trace(tr, buf, v, skin, g);
    }
    hits
}

/// Divide the grid's height across the four card bands.
///
/// Enough room means everyone gets what they asked for. Short means every
/// band keeps at least a header and one honest row (4: borders + header +
/// `… N more`), and the spare goes to the top bands first — recency
/// outranks status, memory's rule. Truly tiny degrades top-down, the old
/// behavior, because eight headers with no rows say less than two whole
/// cards.
fn band_heights<const N: usize>(total: u16, desired: [u16; N]) -> [u16; N] {
    let sum: u16 = desired.iter().sum();
    if sum <= total {
        return desired;
    }
    let floor = 4 * N as u16;
    if total >= floor {
        let mut h = [4u16; N];
        let mut spare = total - floor;
        for i in 0..N {
            let grow = desired[i].saturating_sub(h[i]).min(spare);
            h[i] += grow;
            spare -= grow;
        }
        return h;
    }
    let mut h = [0u16; N];
    let mut left = total;
    for i in 0..N {
        h[i] = desired[i].min(left);
        left -= h[i];
    }
    h
}

/// The clickable action bar: each `[key] Label` pair is one chip — key in a
/// colored box ([`super::palette::Palette::chip`], the approval prompt's
/// dress), label dim beside it. Returns each fully-painted pair's rect with
/// the key it fires; a pair the width clips is painted truncated but never
/// recorded, because a half-visible control is not a control.
fn chip_bar(x: u16, y: u16, w: usize, buf: &mut Buffer, skin: &Skin) -> Vec<(Rect, char)> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut rects = Vec::new();
    let mut used = 0usize;
    for (i, (key, label)) in ACTIONS.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw("  "));
            used += 2;
        }
        let boxed = format!("[{key}]");
        let pair_w = cols(&boxed) + 1 + cols(label);
        if used + pair_w <= w {
            if let Some(c) = key.chars().next() {
                rects.push((Rect::new(x + used as u16, y, pair_w as u16, 1), c));
            }
        }
        used += cols(&boxed) + 1 + cols(label);
        // A control this platform cannot carry out is drawn, and drawn as
        // what it is: the chip loses the accent so it does not read as live.
        // Its rect is still recorded, because clicking it must reach the same
        // answer the key gives rather than being swallowed silently. The
        // words are on the subtitle row above ([`no_controls_note`]), which
        // has the room this one does not.
        let chip = if control_unavailable(key) {
            skin.palette.dim()
        } else {
            skin.palette.chip(Role::Accent)
        };
        spans.push(Span::styled(boxed, chip));
        spans.push(Span::styled(format!(" {label}"), skin.palette.dim()));
    }
    buf.set_line(x, y, &Line::from(spans), w as u16);
    rects
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

// -- the cards ---------------------------------------------------------------

/// One bordered card: the header line (left bold accent, right the card's
/// affordance) under a dim border. Returns the content area under the header.
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

/// Paint a card's footer keybar and record where each chip landed, so a
/// click on `[A] Archive` fires the same action as the letter does.
///
/// The rects come out of the same column arithmetic that painted the line,
/// which is the action bar's rule: two independent measurements of one row are
/// two things that can disagree. A pair whose key is not a single character
/// (the reorder chord) is painted and not recorded, because there is no key
/// for a click to stand in for.
fn footer_bar(
    content: Rect,
    buf: &mut Buffer,
    pairs: &[(&str, &str)],
    sep: &str,
    skin: &Skin,
    chips: &mut Vec<(Rect, char)>,
) {
    let row = content.height - 1;
    set_row(content, row, buf, keybar(pairs, sep, skin));
    let y = content.y + row;
    let mut x = content.x;
    for (i, (key, label)) in pairs.iter().enumerate() {
        if i > 0 {
            x += cols(sep) as u16;
        }
        let text = format!("[{key}] {label}");
        let width = cols(&text) as u16;
        // A chip the card was too narrow to paint is not clickable.
        if x + width > content.x + content.width {
            return;
        }
        let mut chars = key.chars();
        if let (Some(c), None) = (chars.next(), chars.next()) {
            chips.push((Rect::new(x, y, width, 1), c));
        }
        x += width;
    }
}

/// A `[key] Label` affordance bar: accent key, dim label, `sep` between pairs.
fn keybar(pairs: &[(&str, &str)], sep: &str, skin: &Skin) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    for (i, (key, label)) in pairs.iter().enumerate() {
        if i > 0 {
            spans.push(Span::raw(sep.to_string()));
        }
        spans.push(Span::styled(
            format!("[{key}]"),
            skin.palette.style(Role::Accent),
        ));
        spans.push(Span::styled(format!(" {label}"), skin.palette.dim()));
    }
    Line::from(spans)
}

fn render_runs(
    area: Rect,
    buf: &mut Buffer,
    v: &HarnessView,
    skin: &Skin,
    g: &PageGlyphs,
    chips: &mut Vec<(Rect, char)>,
) -> Vec<Rect> {
    let mut boxes = Vec::new();
    if area.width < 6 || area.height < 3 {
        return boxes;
    }
    let affordance = Span::styled(
        format!("View all {}", g.arrow),
        skin.palette.style(Role::Accent),
    );
    let content = card_frame(area, buf, skin, "ACTIVE RUNS", vec![affordance]);
    let w = usize::from(content.width);
    if v.runs.is_empty() {
        set_row(content, 0, buf, empty_line(EMPTY_RUNS, w, skin));
    }
    // The mock's own count: at most three boxes; `View all →` (the [v] key)
    // is the door to the rest, and the tail is counted, never dropped
    // silently.
    for (i, run) in v.runs.iter().take(VISIBLE_RUNS).enumerate() {
        let top = content.y + i as u16 * 5;
        if top + 5 > content.y + content.height {
            break;
        }
        let boxed = Rect::new(content.x, top, content.width, 5);
        let selected = v.selected_run == Some(i);
        let block = Block::bordered()
            .border_set(skin.glyphs.border)
            .border_style(if selected {
                skin.palette.style(Role::Accent)
            } else {
                skin.palette.dim()
            });
        let inner = block.inner(boxed);
        block.render(boxed, buf);
        if inner.width == 0 || inner.height == 0 {
            continue;
        }
        let iw = usize::from(inner.width);
        let (word, style, verb) = match run.state {
            RunState::Running => ("Running", skin.palette.style(Role::Ok), "started"),
            RunState::Paused => ("Paused", skin.palette.style(Role::Warn), "paused"),
            RunState::Completed => ("Completed", skin.palette.dim(), "completed"),
            RunState::Failed => ("Failed", skin.palette.style(Role::Err), "failed"),
        };
        set_row(
            inner,
            0,
            buf,
            lr(
                vec![
                    Span::styled(format!("{} ", g.play), skin.palette.style(Role::Accent)),
                    Span::styled(run.name.clone(), skin.palette.bold(Role::Text)),
                ],
                vec![Span::styled(format!("{} {word}", g.dot), style)],
                iw,
                skin,
            ),
        );
        let second = if run.id.is_empty() {
            format!("{verb} {}", run.stamp)
        } else {
            format!("{} {} {verb} {}", run.id, g.mid, run.stamp)
        };
        set_row(
            inner,
            1,
            buf,
            Line::from(Span::styled(clip(&second, iw, skin), skin.palette.dim())),
        );
        // No ceiling recorded, no gauge: a bar at 0/0 (0%) would be a rate
        // nobody measured (RunRow::progress's own rule). The dash is the
        // page's idiom for an absent value.
        if run.total == 0 {
            set_row(
                inner,
                2,
                buf,
                Line::from(Span::styled(g.endash.to_string(), skin.palette.dim())),
            );
        } else {
            let pct = u64::from(run.done) * 100 / u64::from(run.total);
            let mut spans = gauge(pct, 10, skin, g);
            spans.push(Span::styled(
                format!(" {}/{} ({pct}%)", run.done, run.total),
                skin.palette.style(Role::Text),
            ));
            set_row(inner, 2, buf, Line::from(clip_spans(spans, iw, skin)));
        }
        boxes.push(boxed);
    }
    let hidden = v.runs.len() - boxes.len().min(v.runs.len());
    let used = boxes.len() as u16 * 5 + u16::from(hidden > 0);
    if hidden > 0 {
        set_row(
            content,
            boxes.len() as u16 * 5,
            buf,
            more_line(hidden, w, skin),
        );
    }
    // The footer earns its row only when it is not the count's or a box's.
    if content.height > used {
        footer_bar(
            content,
            buf,
            &[("+", "New Run"), ("A", "Archive"), ("D", "Delete")],
            "   ",
            skin,
            chips,
        );
    }
    boxes
}

fn render_pool(
    area: Rect,
    buf: &mut Buffer,
    v: &HarnessView,
    skin: &Skin,
    g: &PageGlyphs,
    chips: &mut Vec<(Rect, char)>,
) {
    if area.width < 6 || area.height < 3 {
        return;
    }
    let online = v.workers.iter().filter(|x| x.online).count();
    let count = Span::styled(
        format!("{online}/{} online", v.workers.len()),
        skin.palette.dim(),
    );
    let content = card_frame(area, buf, skin, "WORKER POOL", vec![count]);
    let w = usize::from(content.width);
    if v.workers.is_empty() {
        set_row(content, 0, buf, empty_line(EMPTY_WORKERS, w, skin));
    }
    let footer = content.height >= 3;
    let shown = fits(v.workers.len(), content.height - u16::from(footer));
    if shown < v.workers.len() {
        set_row(
            content,
            shown as u16,
            buf,
            more_line(v.workers.len() - shown, w, skin),
        );
    }
    for (i, worker) in v.workers.iter().take(shown).enumerate() {
        let state = if worker.online {
            Span::styled(format!("{} Online", g.check), skin.palette.style(Role::Ok))
        } else {
            Span::styled(
                format!("{} Offline", g.cross),
                skin.palette.style(Role::Err),
            )
        };
        let line = lr(
            vec![
                Span::styled(format!("{} ", g.robot), skin.palette.style(Role::Accent)),
                Span::styled(worker.name.clone(), skin.palette.bold(Role::Text)),
                Span::styled(format!("  {}", worker.sub), skin.palette.dim()),
            ],
            vec![
                state,
                Span::styled(format!("  {}", worker.latency), skin.palette.dim()),
            ],
            w,
            skin,
        );
        set_row(content, i as u16, buf, line);
    }
    if footer {
        // One key, not the mock's three. `[r]` is Resume and `[c]` is Clear
        // Done on this page, and a footer advertising a letter that does
        // something else is the lie this page exists not to tell. `[w]`
        // answers with the fact: there is no pool.
        footer_bar(content, buf, &[("w", "Scale")], "  ", skin, chips);
    }
}

fn render_runtime(area: Rect, buf: &mut Buffer, v: &HarnessView, skin: &Skin, g: &PageGlyphs) {
    if area.width < 4 || area.height < 3 {
        return;
    }
    let content = card_frame(area, buf, skin, "RUNTIME STATUS", Vec::new());
    let w = usize::from(content.width);
    let r = &v.runtime;
    let health = match r.health {
        Some(true) => Span::styled(format!("{} Healthy", g.check), skin.palette.style(Role::Ok)),
        Some(false) => Span::styled(
            format!("{} Unhealthy", g.cross),
            skin.palette.style(Role::Err),
        ),
        None => Span::styled(g.dash.to_string(), skin.palette.dim()),
    };
    let rows: Vec<(&str, Span<'static>)> = vec![
        ("Binary", kv_value(&r.binary, skin, g)),
        ("Version", kv_value(&r.version, skin, g)),
        ("Runtime", kv_value(&r.runtime, skin, g)),
        ("Workspace", kv_value(&r.workspace, skin, g)),
        ("Mode", kv_value(&r.mode, skin, g)),
        ("Model", kv_value(&r.model, skin, g)),
        ("Provider", kv_value(&r.provider, skin, g)),
        ("Started", kv_value(&r.started, skin, g)),
        ("Uptime", kv_value(&r.uptime, skin, g)),
        ("Status", health),
    ];
    let shown = fits(rows.len(), content.height);
    if shown < rows.len() {
        set_row(
            content,
            shown as u16,
            buf,
            more_line(rows.len() - shown, w, skin),
        );
    }
    for (i, (label, value)) in rows.into_iter().take(shown).enumerate() {
        set_row(
            content,
            i as u16,
            buf,
            lr(
                vec![Span::styled(
                    label.to_string(),
                    skin.palette.style(Role::Text),
                )],
                vec![value],
                w,
                skin,
            ),
        );
    }
}

/// The queue and gate tables' fixed right columns, in display columns.
const STATE_COL: usize = 9;
const PROGRESS_COL: usize = 12;

fn render_queue(
    area: Rect,
    buf: &mut Buffer,
    v: &HarnessView,
    skin: &Skin,
    g: &PageGlyphs,
    chips: &mut Vec<(Rect, char)>,
) {
    if area.width < 8 || area.height < 3 {
        return;
    }
    let depth = Span::styled(format!("Depth: {}", v.queue_depth), skin.palette.dim());
    let content = card_frame(area, buf, skin, "TASK QUEUE", vec![depth]);
    let w = usize::from(content.width);
    if content.height <= 1 {
        if v.tasks.is_empty() {
            set_row(content, 0, buf, empty_line(EMPTY_QUEUE, w, skin));
        } else {
            set_row(content, 0, buf, more_line(v.tasks.len(), w, skin));
        }
        return;
    }
    set_row(
        content,
        0,
        buf,
        lr(
            vec![Span::styled(
                format!("{}TASK", pad("#", 4, skin)),
                skin.palette.dim(),
            )],
            vec![Span::styled(
                format!(
                    "{}{}",
                    pad("STATE", STATE_COL, skin),
                    pad("PROGRESS", PROGRESS_COL, skin)
                ),
                skin.palette.dim(),
            )],
            w,
            skin,
        ),
    );
    if v.tasks.is_empty() {
        set_row(content, 1, buf, empty_line(EMPTY_QUEUE, w, skin));
    }
    let footer = content.height >= 4;
    let shown = fits(v.tasks.len(), content.height - 1 - u16::from(footer));
    if shown < v.tasks.len() {
        set_row(
            content,
            shown as u16 + 1,
            buf,
            more_line(v.tasks.len() - shown, w, skin),
        );
    }
    for (i, task) in v.tasks.iter().take(shown).enumerate() {
        let state = match task.state {
            TaskState::Running => Span::styled(
                pad("Running", STATE_COL, skin),
                skin.palette.style(Role::Ok),
            ),
            TaskState::Pending => Span::styled(pad("Pending", STATE_COL, skin), skin.palette.dim()),
            TaskState::Done => Span::styled(pad("Done", STATE_COL, skin), skin.palette.dim()),
        };
        let mut right = vec![state];
        match task.progress_pct {
            Some(pct) => {
                let mut spans = gauge(u64::from(pct.min(100)), 6, skin, g);
                let tail = format!(" {pct}%");
                let used = 6 + cols(&tail);
                spans.push(Span::styled(tail, skin.palette.style(Role::Text)));
                spans.push(Span::raw(" ".repeat(PROGRESS_COL.saturating_sub(used))));
                right.extend(spans);
            }
            None => right.push(Span::styled(
                pad(g.endash, PROGRESS_COL, skin),
                skin.palette.dim(),
            )),
        }
        let mut line = lr(
            vec![
                Span::styled(pad(&task.n.to_string(), 4, skin), skin.palette.dim()),
                Span::styled(task.name.clone(), skin.palette.style(Role::Text)),
            ],
            right,
            w,
            skin,
        );
        if v.selected_task == Some(i) {
            line = banded(line, skin.palette.chip(Role::Accent));
        }
        set_row(content, i as u16 + 1, buf, line);
    }
    if footer {
        // Shift on the arrows: the plain ones are the selection's, in
        // whichever card has focus.
        footer_bar(
            content,
            buf,
            &[
                ("n", "New"),
                (&format!("{}{}", g.shift, g.updown), "Reorder"),
                ("c", "Clear Done"),
            ],
            "  ",
            skin,
            chips,
        );
    }
}

fn render_gates(
    area: Rect,
    buf: &mut Buffer,
    v: &HarnessView,
    skin: &Skin,
    g: &PageGlyphs,
    chips: &mut Vec<(Rect, char)>,
) {
    if area.width < 8 || area.height < 3 {
        return;
    }
    // Plan mode outranks the gate: see [`GATE_PLAN`]. `Mode::label`'s own
    // spelling, so the two cannot come to disagree about the word.
    let posture = if v.mode_label == crate::approval::Mode::Plan.label() {
        GATE_PLAN.to_string()
    } else if v.auto_approve.is_empty() {
        g.dash.to_string()
    } else {
        v.auto_approve.clone()
    };
    let posture = Span::styled(format!("Auto-approve: {posture}"), skin.palette.dim());
    let content = card_frame(area, buf, skin, "TOOL GATES / APPROVALS", vec![posture]);
    let w = usize::from(content.width);
    // One content row is not enough for chrome: it goes to the honest count
    // (or the empty state), never to a footer painted over a column header.
    if content.height <= 1 {
        if v.gates.is_empty() {
            set_row(content, 0, buf, empty_line(EMPTY_GATES, w, skin));
        } else {
            set_row(content, 0, buf, more_line(v.gates.len(), w, skin));
        }
        return;
    }
    set_row(
        content,
        0,
        buf,
        lr(
            vec![Span::styled(
                format!("{}REQUEST", pad("TOOL", 12, skin)),
                skin.palette.dim(),
            )],
            vec![Span::styled("STATUS".to_string(), skin.palette.dim())],
            w,
            skin,
        ),
    );
    if v.gates.is_empty() {
        set_row(content, 1, buf, empty_line(EMPTY_GATES, w, skin));
    }
    // The footer earns its row only once a data row also has one.
    let footer = content.height >= 4;
    let shown = fits(v.gates.len(), content.height - 1 - u16::from(footer));
    if shown < v.gates.len() {
        set_row(
            content,
            shown as u16 + 1,
            buf,
            more_line(v.gates.len() - shown, w, skin),
        );
    }
    for (i, gate) in v.gates.iter().take(shown).enumerate() {
        let status = match gate.status {
            GateStatus::Allowed => {
                Span::styled(format!("{} Allowed", g.check), skin.palette.style(Role::Ok))
            }
            GateStatus::Pending => Span::styled(
                format!("{} Pending", g.hourglass),
                skin.palette.style(Role::Warn),
            ),
        };
        let line = lr(
            vec![
                Span::styled(pad(&gate.tool, 12, skin), skin.palette.bold(Role::Text)),
                Span::styled(gate.request.clone(), skin.palette.dim()),
            ],
            vec![status],
            w,
            skin,
        );
        set_row(content, i as u16 + 1, buf, line);
    }
    if footer {
        // `[s]`, not the mock's `[a]`: `[a]` is Run in the action bar above,
        // and one letter cannot mean two things on one page.
        footer_bar(
            content,
            buf,
            &[
                ("t", "Show"),
                ("s", "Safe"),
                ("m", "Manual"),
                ("d", "Deny All"),
            ],
            "  ",
            skin,
            chips,
        );
    }
}

fn render_resources(area: Rect, buf: &mut Buffer, v: &HarnessView, skin: &Skin, g: &PageGlyphs) {
    if area.width < 4 || area.height < 3 {
        return;
    }
    let r = &v.resources;
    let live = if r.live {
        Span::styled("Live".to_string(), skin.palette.style(Role::Accent))
    } else {
        Span::styled(g.dash.to_string(), skin.palette.dim())
    };
    let content = card_frame(area, buf, skin, "RESOURCES", vec![live]);
    let w = usize::from(content.width);
    // The two gauge rows first, then the plain kv rows.
    let gauged = |pct: Option<u8>, text: Span<'static>| -> Vec<Span<'static>> {
        match pct {
            Some(p) => {
                let mut spans = gauge(u64::from(p.min(100)), 10, skin, g);
                spans.push(Span::raw(" "));
                spans.push(text);
                spans
            }
            None => vec![text],
        }
    };
    let cpu_text = match r.cpu_pct {
        Some(p) => Span::styled(format!("{p}%"), skin.palette.style(Role::Accent)),
        None => Span::styled(g.dash.to_string(), skin.palette.dim()),
    };
    let rows: Vec<(&str, Vec<Span<'static>>)> = vec![
        ("CPU Usage", gauged(r.cpu_pct, cpu_text)),
        ("Memory Usage", gauged(r.mem_pct, kv_value(&r.mem, skin, g))),
        ("Disk IO (R/W)", vec![kv_value(&r.disk_io, skin, g)]),
        ("Network (In/Out)", vec![kv_value(&r.network, skin, g)]),
        ("Workers", vec![kv_value(&r.workers, skin, g)]),
        ("Queue Depth", vec![kv_value(&r.queue_depth, skin, g)]),
        ("Avg Task Latency", vec![kv_value(&r.avg_latency, skin, g)]),
        ("P95 Latency", vec![kv_value(&r.p95_latency, skin, g)]),
        ("Retries (last 1h)", vec![kv_value(&r.retries, skin, g)]),
        ("Span Sample Rate", vec![kv_value(&r.span_rate, skin, g)]),
    ];
    let shown = fits(rows.len(), content.height);
    if shown < rows.len() {
        set_row(
            content,
            shown as u16,
            buf,
            more_line(rows.len() - shown, w, skin),
        );
    }
    for (i, (label, value)) in rows.into_iter().take(shown).enumerate() {
        set_row(
            content,
            i as u16,
            buf,
            lr(
                vec![Span::styled(
                    label.to_string(),
                    skin.palette.style(Role::Text),
                )],
                value,
                w,
                skin,
            ),
        );
    }
}

fn render_events(area: Rect, buf: &mut Buffer, v: &HarnessView, skin: &Skin, g: &PageGlyphs) {
    if area.width < 6 || area.height < 3 {
        return;
    }
    let affordance = Span::styled(
        format!("View full {}", g.arrow),
        skin.palette.style(Role::Accent),
    );
    let content = card_frame(area, buf, skin, "EVENT LOG", vec![affordance]);
    let w = usize::from(content.width);
    if v.events.is_empty() {
        set_row(content, 0, buf, empty_line(EMPTY_EVENTS, w, skin));
        return;
    }
    let shown = fits(v.events.len(), content.height);
    if shown < v.events.len() {
        set_row(
            content,
            shown as u16,
            buf,
            more_line(v.events.len() - shown, w, skin),
        );
    }
    for (i, e) in v.events.iter().take(shown).enumerate() {
        let (word, style) = match e.level {
            LogLevel::Info => ("INFO", skin.palette.style(Role::Info)),
            LogLevel::Warn => ("WARN", skin.palette.style(Role::Warn)),
            LogLevel::Error => ("ERROR", skin.palette.style(Role::Err)),
        };
        let line = Line::from(clip_spans(
            vec![
                Span::styled(format!("{}  ", e.time), skin.palette.dim()),
                Span::styled(pad(word, 6, skin), style),
                Span::styled(e.text.clone(), skin.palette.style(Role::Text)),
            ],
            w,
            skin,
        ));
        set_row(content, i as u16, buf, line);
    }
}

fn render_trace(area: Rect, buf: &mut Buffer, v: &HarnessView, skin: &Skin, g: &PageGlyphs) {
    if area.width < 4 || area.height < 3 {
        return;
    }
    let right = if v.current_run.is_empty() {
        Vec::new()
    } else {
        vec![Span::styled(
            format!("Current Run: {}", v.current_run),
            skin.palette
                .style(Role::Accent)
                .add_modifier(Modifier::UNDERLINED),
        )]
    };
    let content = card_frame(area, buf, skin, "TRACE / CONTEXT", right);
    let w = usize::from(content.width);
    let t = &v.trace;
    let rows: Vec<(&str, Span<'static>)> = vec![
        ("Trace ID", kv_value(&t.trace_id, skin, g)),
        ("Parent Span", kv_value(&t.parent_span, skin, g)),
        ("Total Spans", kv_value(&t.total_spans, skin, g)),
        ("Active Spans", kv_value(&t.active_spans, skin, g)),
        ("Last Span", kv_value(&t.last_span, skin, g)),
        ("Context Size", kv_value(&t.context_size, skin, g)),
        ("Prompt Cache", kv_value(&t.prompt_cache, skin, g)),
    ];
    let shown = fits(rows.len(), content.height);
    if shown < rows.len() {
        set_row(
            content,
            shown as u16,
            buf,
            more_line(rows.len() - shown, w, skin),
        );
    }
    for (i, (label, value)) in rows.into_iter().take(shown).enumerate() {
        set_row(
            content,
            i as u16,
            buf,
            lr(
                vec![Span::styled(
                    label.to_string(),
                    skin.palette.style(Role::Text),
                )],
                vec![value],
                w,
                skin,
            ),
        );
    }
}

fn render_input(area: Rect, buf: &mut Buffer, v: &HarnessView, skin: &Skin) {
    let block = Block::bordered()
        .border_set(skin.glyphs.border)
        .border_style(skin.palette.style(Role::Accent));
    let inner = block.inner(area);
    block.render(area, buf);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let text = if v.command.is_empty() {
        Span::styled(PLACEHOLDER.to_string(), skin.palette.dim())
    } else {
        Span::styled(v.command.clone(), skin.palette.style(Role::Text))
    };
    let line = lr(
        vec![
            Span::styled("> ".to_string(), skin.palette.bold(Role::Accent)),
            text,
        ],
        vec![
            Span::styled("[Enter]".to_string(), skin.palette.style(Role::Accent)),
            Span::styled(" Execute".to_string(), skin.palette.dim()),
        ],
        usize::from(inner.width),
        skin,
    );
    buf.set_line(inner.x, inner.y, &line, inner.width);
}

// -- span carpentry ----------------------------------------------------------

/// A kv row's right-hand value: accent when present, the honest dash when
/// the backing feed does not exist yet.
fn kv_value(text: &str, skin: &Skin, g: &PageGlyphs) -> Span<'static> {
    if text.is_empty() {
        Span::styled(g.dash.to_string(), skin.palette.dim())
    } else {
        Span::styled(text.to_string(), skin.palette.style(Role::Accent))
    }
}

/// The status bar's gauge idiom: `segments` cells, ceil fill capped one
/// short of full below 100%, filled accent, empty dim.
fn gauge(pct: u64, segments: usize, skin: &Skin, g: &PageGlyphs) -> Vec<Span<'static>> {
    let pct = pct.min(100) as usize;
    let filled = if pct >= 100 {
        segments
    } else {
        (pct * segments)
            .div_ceil(100)
            .min(segments.saturating_sub(1))
    };
    let mut spans = Vec::new();
    if filled > 0 {
        spans.push(Span::styled(
            g.full.repeat(filled),
            skin.palette.style(Role::Accent),
        ));
    }
    if filled < segments {
        spans.push(Span::styled(
            g.empty.repeat(segments - filled),
            skin.palette.dim(),
        ));
    }
    spans
}

/// Left spans, a gap, right spans, at exactly `w` columns. The right side
/// survives and the left truncates — the sidebar's trailing-column ruling.
fn lr(left: Vec<Span<'static>>, right: Vec<Span<'static>>, w: usize, skin: &Skin) -> Line<'static> {
    let rw: usize = right.iter().map(|s| cols(&s.content)).sum();
    let budget = w.saturating_sub(rw + 1);
    let mut out = clip_spans(left, budget, skin);
    let used: usize = out.iter().map(|s| cols(&s.content)).sum();
    out.push(Span::raw(" ".repeat(w.saturating_sub(used + rw))));
    out.extend(right);
    Line::from(out)
}

/// `spans`, truncated as a group to `budget` display columns.
fn clip_spans(spans: Vec<Span<'static>>, budget: usize, skin: &Skin) -> Vec<Span<'static>> {
    let mut out: Vec<Span<'static>> = Vec::new();
    let mut used = 0usize;
    for s in spans {
        let cw = cols(&s.content);
        if used + cw <= budget {
            used += cw;
            out.push(s);
        } else {
            let room = budget.saturating_sub(used);
            if room > 0 {
                let t = clip(&s.content, room, skin);
                out.push(Span::styled(t, s.style));
            }
            break;
        }
    }
    out
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

/// `text` clipped then space-padded to exactly `w` display columns — the
/// table columns' alignment.
fn pad(text: &str, w: usize, skin: &Skin) -> String {
    let t = clip(text, w, skin);
    let used = cols(&t);
    format!("{t}{}", " ".repeat(w.saturating_sub(used)))
}

/// Paint one content row, clipped to the content area.
fn set_row(content: Rect, i: u16, buf: &mut Buffer, line: Line<'static>) {
    if i < content.height {
        buf.set_line(content.x, content.y + i, &line, content.width);
    }
}

/// [`fit`], made total for budgets narrower than the ellipsis — the memory
/// screen's `clip`, duplicated for the same reason it duplicated the
/// settings screen's: the original is private to its module.
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
pub(crate) mod tests {
    use super::super::palette::{Level, Palette, Role};
    use super::super::render::{cols, ASCII, UNICODE};
    use super::*;
    use ratatui::style::{Color, Modifier};

    fn skin() -> Skin {
        Skin::new(Palette::new(Level::Truecolor), UNICODE)
    }

    /// The mock's RESOURCES numbers, in one place.
    ///
    /// A function rather than a literal inside `populated` because a live
    /// view must be able to be checked against it: a test elsewhere can assert
    /// that not one of these values reaches a card built from a real session
    /// directory, and a copy of them there would go stale the moment somebody
    /// edited the mock.
    pub(crate) fn mock_resources() -> ResourcesView {
        ResourcesView {
            cpu_pct: Some(24),
            mem: "3.2 GB / 16 GB".into(),
            mem_pct: Some(20),
            disk_io: "12 MB/s / 8 MB/s".into(),
            network: "1.2 Mb/s / 0.9 Mb/s".into(),
            workers: "4/4".into(),
            queue_depth: "5".into(),
            avg_latency: "1.23s".into(),
            p95_latency: "2.87s".into(),
            retries: "3".into(),
            span_rate: "20%".into(),
            live: true,
        }
    }

    /// The mock's sample data, in full. This is what the page looks like once
    /// the harness backend exists; the fidelity is pinned now. The WARN
    /// event's time is invented (the transcription names none) — flagged in
    /// the design note.
    fn populated() -> HarnessView {
        HarnessView {
            version: "v0.6.3".into(),
            runs: vec![
                Run {
                    name: "checkout-triage".into(),
                    state: RunState::Running,
                    id: "jd93f2a1".into(),
                    key: "sess-mock#1".into(),
                    stamp: "10:21:34".into(),
                    done: 4,
                    total: 9,
                },
                Run {
                    name: "api-degradation-investigation".into(),
                    state: RunState::Paused,
                    id: String::new(),
                    key: "sess-mock#2".into(),
                    stamp: "09:53:11".into(),
                    done: 6,
                    total: 12,
                },
                Run {
                    name: "rollback-procedure".into(),
                    state: RunState::Completed,
                    id: String::new(),
                    key: "sess-mock#3".into(),
                    stamp: "09:12:08".into(),
                    done: 12,
                    total: 12,
                },
            ],
            selected_run: Some(0),
            workers: vec![
                Worker {
                    name: "orchestrator".into(),
                    sub: "tokio".into(),
                    online: true,
                    latency: "0.8s".into(),
                },
                Worker {
                    name: "coder".into(),
                    sub: "rust-analyzer".into(),
                    online: true,
                    latency: "1.1s".into(),
                },
                Worker {
                    name: "reviewer".into(),
                    sub: "clippy".into(),
                    online: true,
                    latency: "0.6s".into(),
                },
                Worker {
                    name: "tester".into(),
                    sub: "cargo test".into(),
                    online: true,
                    latency: "1.4s".into(),
                },
            ],
            runtime: RuntimeView {
                binary: "emma-harness".into(),
                version: "v0.6.3".into(),
                runtime: "tokio 1.37".into(),
                workspace: "~/Projects/emma".into(),
                mode: "local".into(),
                model: "llama3:8b (local)".into(),
                provider: "ollama".into(),
                started: "2h 18m ago".into(),
                uptime: "2h 18m 24s".into(),
                health: Some(true),
            },
            tasks: vec![
                Task {
                    n: 1,
                    id: String::new(),
                    name: "Verify alert details".into(),
                    state: TaskState::Running,
                    progress_pct: Some(70),
                },
                Task {
                    n: 2,
                    id: String::new(),
                    name: "Check service health".into(),
                    state: TaskState::Pending,
                    progress_pct: None,
                },
                Task {
                    n: 3,
                    id: String::new(),
                    name: "Review error rate".into(),
                    state: TaskState::Pending,
                    progress_pct: None,
                },
                Task {
                    n: 4,
                    id: String::new(),
                    name: "Identify customer impact".into(),
                    state: TaskState::Running,
                    progress_pct: Some(40),
                },
                Task {
                    n: 5,
                    id: String::new(),
                    name: "Check dependencies".into(),
                    state: TaskState::Pending,
                    progress_pct: None,
                },
                Task {
                    n: 9,
                    id: String::new(),
                    name: "Document findings".into(),
                    state: TaskState::Pending,
                    progress_pct: None,
                },
            ],
            selected_task: Some(3),
            queue_depth: 5,
            auto_approve: "Safe".into(),
            gates: vec![
                Gate {
                    tool: "Shell".into(),
                    request: "kubectl logs -n prod -l app=checkout".into(),
                    status: GateStatus::Allowed,
                },
                Gate {
                    tool: "File Write".into(),
                    request: "write src/handlers/checkout.rs".into(),
                    status: GateStatus::Pending,
                },
                Gate {
                    tool: "Git".into(),
                    request: "commit changes to feature/triage".into(),
                    status: GateStatus::Pending,
                },
                Gate {
                    tool: "Web Fetch".into(),
                    request: "GET https://status.payment-gateway.com".into(),
                    status: GateStatus::Allowed,
                },
                Gate {
                    tool: "Memory".into(),
                    request: "store session summary".into(),
                    status: GateStatus::Allowed,
                },
            ],
            resources: mock_resources(),
            events: vec![
                Event {
                    time: "12:46:21".into(),
                    level: LogLevel::Info,
                    text: "Run jd93f2a1 started: checkout-triage".into(),
                },
                Event {
                    time: "12:46:29".into(),
                    level: LogLevel::Warn,
                    text: "Tool FileWrite requires approval: src/handlers/checkout.rs".into(),
                },
            ],
            trace: TraceView {
                trace_id: "8f3c2a1b7d4e9c12".into(),
                parent_span: "4d8e12a7c3b9f001".into(),
                total_spans: "37".into(),
                active_spans: "12".into(),
                last_span: "12:46:33".into(),
                context_size: "18,432 tokens (23%)".into(),
                prompt_cache: "Enabled (87% hit rate)".into(),
            },
            current_run: "jd93f2a1".into(),
            command: String::new(),
            ..HarnessView::default()
        }
    }

    /// Today's truth: no harness backend, nothing live but the runtime trio.
    fn empty() -> HarnessView {
        HarnessView {
            version: "v0.6.3".into(),
            runtime: RuntimeView {
                binary: "emma".into(),
                version: "v0.6.3".into(),
                workspace: "~/Projects/emma".into(),
                ..RuntimeView::default()
            },
            ..HarnessView::default()
        }
    }

    fn buffer(v: &HarnessView, w: u16, h: u16) -> Buffer {
        let area = Rect::new(0, 0, w, h);
        let mut buf = Buffer::empty(area);
        render(area, &mut buf, v, &skin());
        buf
    }

    fn lines(buf: &Buffer) -> Vec<String> {
        // A double-width glyph (the worker rows' robot, the gates' hourglass)
        // owns two cells; the continuation cell reads back as a space and
        // would double-count the column, so it is skipped.
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

    fn draw(v: &HarnessView, w: u16, h: u16) -> Vec<String> {
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

    /// One card's slice of a row. Three cards share a row in the mock's top
    /// bands, so a plain `row.find` can match the neighbour's column; this
    /// cuts the row at the card gaps and returns the piece holding `needle`.
    fn card_slice<'a>(row: &'a str, needle: &str) -> &'a str {
        for part in row.split("│ │") {
            if part.contains(needle) {
                return part;
            }
        }
        panic!("{needle:?} not in {row:?}");
    }

    /// `right` ends flush against the next card border (or the row's end),
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

    fn warn() -> Color {
        skin().palette.color(Role::Warn)
    }

    // -- the head and the action bar -----------------------------------------

    #[test]
    fn the_head_is_version_title_subtitle_rule() {
        let rows = draw(&populated(), 161, 75);
        assert!(
            rows[0].ends_with("v0.6.3"),
            "version not in the corner: {:?}",
            rows[0]
        );
        assert_eq!(rows[1], "Harness");
        // The subtitle row also carries the platform's refusal, flush right,
        // where there is one to carry: see `no_controls_note`.
        assert!(rows[2].starts_with(SUBTITLE), "subtitle: {:?}", rows[2]);
        match no_controls_note() {
            Some(note) => assert!(rows[2].ends_with(&note), "note: {:?}", rows[2]),
            None => assert_eq!(rows[2], SUBTITLE),
        }
        assert!(
            rows[3].chars().all(|c| c == '─') && !rows[3].is_empty(),
            "rule missing"
        );
    }

    #[test]
    fn the_action_bar_is_the_mocks_exactly() {
        let rows = draw(&populated(), 161, 75);
        let bar = row_with(&rows, "[a] Run");
        assert_eq!(
            bar.trim_end(),
            "[a] Run  [p] Pause  [r] Resume  [x] Cancel  [i] Inspect  [l] Logs  [g] Graph  [R] Refresh  [?] Help"
        );
    }

    // -- card headers and their right affordances -----------------------------

    #[test]
    fn card_headers_carry_the_mocks_right_affordances() {
        let rows = draw(&populated(), 161, 75);
        let runs = row_with(&rows, "ACTIVE RUNS");
        assert!(
            runs.contains("View all →"),
            "View all affordance missing: {runs:?}"
        );
        let pool = row_with(&rows, "WORKER POOL");
        assert!(
            pool.contains("4/4 online"),
            "online count missing: {pool:?}"
        );
        assert_eq!(runs, pool, "top pair does not share the header band");
        row_with(&rows, "RUNTIME STATUS");
        let queue = row_with(&rows, "TASK QUEUE");
        assert!(queue.contains("Depth: 5"), "queue depth missing: {queue:?}");
        let gates = row_with(&rows, "TOOL GATES / APPROVALS");
        assert!(
            gates.contains("Auto-approve: Safe"),
            "posture missing: {gates:?}"
        );
        let res = row_with(&rows, "RESOURCES");
        assert!(res.contains("Live"), "Live claim missing: {res:?}");
        let log = row_with(&rows, "EVENT LOG");
        assert!(
            log.contains("View full →"),
            "View full affordance missing: {log:?}"
        );
        let trace = row_with(&rows, "TRACE / CONTEXT");
        assert!(
            trace.contains("Current Run: jd93f2a1"),
            "current run missing: {trace:?}"
        );
    }

    #[test]
    fn cards_sit_in_the_mocks_three_columns() {
        let rows = draw(&populated(), 161, 75);
        // Thirds, with the two air columns between them: column 0 starts at
        // the left edge, 1 near a third across, 2 near two thirds.
        let third = 161 / 3;
        for (needle, col) in [
            ("ACTIVE RUNS", 0),
            ("WORKER POOL", 1),
            ("RUNTIME STATUS", 2),
            ("TASK QUEUE", 0),
            ("TOOL GATES / APPROVALS", 1),
            ("RESOURCES", 2),
        ] {
            let x = rows
                .iter()
                .find_map(|r| r.find(needle))
                .unwrap_or_else(|| panic!("{needle} not rendered"));
            assert_eq!(x / third, col, "{needle} in the wrong column (x={x})");
        }
        // The bottom row of two: EVENT LOG wide on the left, TRACE right.
        let mid = 161 / 2;
        for (needle, left) in [("EVENT LOG", true), ("TRACE / CONTEXT", false)] {
            let x = rows
                .iter()
                .find_map(|r| r.find(needle))
                .unwrap_or_else(|| panic!("{needle} not rendered"));
            assert_eq!(x < mid, left, "{needle} in the wrong column (x={x})");
        }
    }

    // -- ACTIVE RUNS, per mock ------------------------------------------------

    #[test]
    fn run_boxes_carry_name_status_id_line_and_gauge() {
        let rows = draw(&populated(), 161, 75);
        let head = row_with(&rows, "checkout-triage");
        assert!(head.contains("● Running"), "status missing: {head:?}");
        let inner = head.trim_start_matches(['│', ' ']);
        assert!(inner.starts_with('▶'), "play glyph missing: {head:?}");
        row_with(&rows, "jd93f2a1 · started 10:21:34");
        let gauge = row_with(&rows, "4/9 (44%)");
        assert!(
            gauge.contains('█') && gauge.contains('░'),
            "gauge missing: {gauge:?}"
        );
        let paused = row_with(&rows, "api-degradation-investigation");
        assert!(
            paused.contains("● Paused"),
            "paused status missing: {paused:?}"
        );
        row_with(&rows, "paused 09:53:11");
        row_with(&rows, "6/12 (50%)");
        let done = row_with(&rows, "rollback-procedure");
        assert!(
            done.contains("● Completed"),
            "completed status missing: {done:?}"
        );
        row_with(&rows, "completed 09:12:08");
        let full = row_with(&rows, "12/12 (100%)");
        assert!(
            full.contains('█') && !full.contains('░'),
            "100% gauge not full: {full:?}"
        );
    }

    #[test]
    fn the_selected_runs_box_border_takes_the_accent() {
        let buf = buffer(&populated(), 161, 75);
        let rows = lines(&buf);
        let (x1, y1) = locate(&rows, "checkout-triage");
        assert_eq!(
            buf[(x1, y1 - 1)].style().fg,
            Some(accent()),
            "selected run's box border not accent"
        );
        let (x2, y2) = locate(&rows, "api-degradation-investigation");
        assert_ne!(
            buf[(x2, y2 - 1)].style().fg,
            Some(accent()),
            "accent leaked to an unselected run's box"
        );
    }

    #[test]
    fn the_runs_footer_is_the_mocks_exactly() {
        let rows = draw(&populated(), 161, 75);
        let footer = row_with(&rows, "[+] New Run");
        assert!(
            footer.contains("[+] New Run   [A] Archive   [D] Delete"),
            "runs footer wrong: {footer:?}"
        );
    }

    // -- WORKER POOL, per mock ------------------------------------------------

    #[test]
    fn worker_rows_are_glyph_name_sub_and_right_status() {
        let rows = draw(&populated(), 161, 75);
        for (name, sub, latency) in [
            ("orchestrator", "tokio", "0.8s"),
            ("coder", "rust-analyzer", "1.1s"),
            ("reviewer", "clippy", "0.6s"),
            ("tester", "cargo test", "1.4s"),
        ] {
            let row = row_with(&rows, name);
            assert!(
                row.contains(sub),
                "{sub:?} missing beside {name:?}: {row:?}"
            );
            assert!(row.contains("✓ Online"), "online mark missing: {row:?}");
            assert!(row.contains(latency), "{latency:?} missing: {row:?}");
            flush_right_of(row, latency);
        }
        // One key, not the mock's three: `[r]` and `[c]` already mean Resume
        // and Clear Done on this page. See `render_pool`.
        let footer = row_with(&rows, "[w] Scale");
        assert!(
            !footer.contains("[r] Restart"),
            "pool footer advertises a taken key: {footer:?}"
        );
        assert!(
            !footer.contains("[c] Config"),
            "pool footer advertises a taken key: {footer:?}"
        );
    }

    // -- RUNTIME STATUS, per mock ---------------------------------------------

    #[test]
    fn runtime_status_rows_carry_the_mocks_labels_and_flush_values() {
        let rows = draw(&populated(), 161, 75);
        for (label, value) in [
            ("Binary", "emma-harness"),
            ("Version", "v0.6.3"),
            ("Runtime", "tokio 1.37"),
            ("Workspace", "~/Projects/emma"),
            ("Mode", "local"),
            ("Model", "llama3:8b (local)"),
            ("Provider", "ollama"),
            ("Started", "2h 18m ago"),
            ("Uptime", "2h 18m 24s"),
        ] {
            let row = card_slice(row_with(&rows, label), label);
            assert!(
                row.contains(value),
                "{value:?} missing beside {label:?}: {row:?}"
            );
            flush_right_of(row, value);
        }
        let status = row_with(&rows, "Status");
        assert!(
            status.contains("✓ Healthy"),
            "health mark missing: {status:?}"
        );
        flush_right_of(status, "✓ Healthy");
    }

    // -- TASK QUEUE, per mock -------------------------------------------------

    #[test]
    fn the_task_queue_is_a_numbered_table_with_progress() {
        let rows = draw(&populated(), 161, 75);
        let head = row_with(&rows, "PROGRESS");
        assert!(
            head.contains('#') && head.contains("TASK") && head.contains("STATE"),
            "column headers missing: {head:?}"
        );
        let running = row_with(&rows, "Verify alert details");
        assert!(running.contains("Running"), "state missing: {running:?}");
        assert!(running.contains("70%"), "progress missing: {running:?}");
        assert!(running.contains('█'), "mini-gauge missing: {running:?}");
        let pending = row_with(&rows, "Check service health");
        assert!(pending.contains("Pending"), "state missing: {pending:?}");
        assert!(
            pending.contains('–'),
            "en-dash placeholder missing: {pending:?}"
        );
        assert!(
            !pending.contains('█'),
            "pending row claims progress: {pending:?}"
        );
        row_with(&rows, "Review error rate");
        let banded = row_with(&rows, "Identify customer impact");
        assert!(banded.contains("40%"), "progress missing: {banded:?}");
        row_with(&rows, "Check dependencies");
        let last = row_with(&rows, "Document findings");
        assert!(
            last.contains("9   Document findings"),
            "task 9 not numbered 9: {last:?}"
        );
        let footer = row_with(&rows, "[n] New");
        assert!(
            footer.contains("[n] New  [⇧↑/↓] Reorder  [c] Clear Done"),
            "queue footer wrong: {footer:?}"
        );
    }

    #[test]
    fn the_selected_task_row_wears_the_band_and_only_that_row() {
        let buf = buffer(&populated(), 161, 75);
        let rows = lines(&buf);
        let (x4, y4) = locate(&rows, "Identify customer impact");
        assert_eq!(
            buf[(x4, y4)].style().bg,
            Some(accent()),
            "no band on the selected task"
        );
        let (x1, y1) = locate(&rows, "Verify alert details");
        assert_ne!(
            buf[(x1, y1)].style().bg,
            Some(accent()),
            "band leaked to another task"
        );
    }

    // -- TOOL GATES, per mock -------------------------------------------------

    #[test]
    fn tool_gates_rows_carry_tool_request_and_status() {
        let rows = draw(&populated(), 220, 75);
        let head = row_with(&rows, "REQUEST");
        assert!(
            head.contains("TOOL") && head.contains("STATUS"),
            "gate headers missing: {head:?}"
        );
        let shell = row_with(&rows, "kubectl logs -n prod -l app=checkout");
        assert!(shell.contains("Shell"), "tool missing: {shell:?}");
        assert!(
            shell.contains("✓ Allowed"),
            "allowed mark missing: {shell:?}"
        );
        flush_right_of(shell, "✓ Allowed");
        let fw = row_with(&rows, "write src/handlers/checkout.rs");
        assert!(fw.contains("File Write"), "tool missing: {fw:?}");
        assert!(fw.contains("⏳ Pending"), "pending mark missing: {fw:?}");
        let git = row_with(&rows, "commit changes to feature/triage");
        assert!(git.contains("⏳ Pending"), "pending mark missing: {git:?}");
        row_with(&rows, "GET https://status.payment-gateway.com");
        let mem = row_with(&rows, "store session summary");
        assert!(mem.contains("✓ Allowed"), "allowed mark missing: {mem:?}");
        let footer = row_with(&rows, "[t] Show");
        assert!(
            // `[s]`, not the mock's `[a]`: `[a]` is Run in the action bar.
            footer.contains("[t] Show  [s] Safe  [m] Manual  [d] Deny All"),
            "gates footer wrong: {footer:?}"
        );
    }

    #[test]
    fn a_pending_gate_is_amber() {
        let buf = buffer(&populated(), 220, 75);
        let rows = lines(&buf);
        let (_, y) = locate(&rows, "write src/handlers/checkout.rs");
        let row = &rows[usize::from(y)];
        // The gate card's own Pending, not the task queue's on the same row.
        let gate = card_slice(row, "write src/handlers/checkout.rs");
        let at = row.find(gate).expect("slice came from this row")
            + gate.find("Pending").expect("pending missing");
        let px = cols(&row[..at]) as u16;
        assert_eq!(
            buf[(px, y)].style().fg,
            Some(warn()),
            "pending status not amber"
        );
    }

    // -- RESOURCES, per mock --------------------------------------------------

    #[test]
    fn the_live_badge_needs_every_row_and_a_partial_card_never_wears_it() {
        assert!(mock_resources().complete(), "the mock fills every row");
        let mut partial = mock_resources();
        partial.disk_io.clear();
        assert!(!partial.complete(), "one placeholder must sink the badge");
        assert!(!ResourcesView::default().complete());

        // And the paint follows the flag: a card with `live` off shows the
        // dash where the badge would be.
        let mut v = populated();
        v.resources.live = false;
        let rows = draw(&v, 220, 75);
        assert!(
            !row_with(&rows, "RESOURCES").contains("Live"),
            "the badge outlived the claim"
        );
    }

    #[test]
    fn resources_rows_carry_the_mocks_labels_gauges_and_flush_values() {
        let rows = draw(&populated(), 220, 75);
        let cpu = card_slice(row_with(&rows, "CPU Usage"), "CPU Usage");
        assert!(cpu.contains('█'), "cpu gauge missing: {cpu:?}");
        flush_right_of(cpu, "24%");
        let mem = card_slice(row_with(&rows, "Memory Usage"), "Memory Usage");
        assert!(mem.contains('█'), "memory gauge missing: {mem:?}");
        flush_right_of(mem, "3.2 GB / 16 GB");
        for (label, value) in [
            ("Disk IO (R/W)", "12 MB/s / 8 MB/s"),
            ("Network (In/Out)", "1.2 Mb/s / 0.9 Mb/s"),
            ("Workers", "4/4"),
            ("Queue Depth", "5"),
            ("Avg Task Latency", "1.23s"),
            ("P95 Latency", "2.87s"),
            ("Retries (last 1h)", "3"),
            ("Span Sample Rate", "20%"),
        ] {
            let row = card_slice(row_with(&rows, label), label);
            assert!(
                row.contains(value),
                "{value:?} missing beside {label:?}: {row:?}"
            );
            flush_right_of(row, value);
        }
    }

    // -- EVENT LOG and TRACE / CONTEXT, per mock ------------------------------

    #[test]
    fn event_rows_are_time_level_text_and_warn_is_amber() {
        let buf = buffer(&populated(), 161, 75);
        let rows = lines(&buf);
        let info = row_with(&rows, "Run jd93f2a1 started: checkout-triage");
        assert!(info.contains("12:46:21"), "time missing: {info:?}");
        assert!(info.contains("INFO"), "level missing: {info:?}");
        let warn_row = row_with(&rows, "Tool FileWrite requires approval");
        assert!(warn_row.contains("WARN"), "level missing: {warn_row:?}");
        let (_, y) = locate(&rows, "Tool FileWrite requires approval");
        let row = &rows[usize::from(y)];
        let wx = cols(&row[..row.find("WARN").expect("WARN missing")]) as u16;
        assert_eq!(
            buf[(wx, y)].style().fg,
            Some(warn()),
            "WARN level not amber"
        );
    }

    #[test]
    fn trace_rows_carry_the_mocks_labels_and_flush_values() {
        let buf = buffer(&populated(), 161, 75);
        let rows = lines(&buf);
        for (label, value) in [
            ("Trace ID", "8f3c2a1b7d4e9c12"),
            ("Parent Span", "4d8e12a7c3b9f001"),
            ("Total Spans", "37"),
            ("Active Spans", "12"),
            ("Last Span", "12:46:33"),
            ("Context Size", "18,432 tokens (23%)"),
            ("Prompt Cache", "Enabled (87% hit rate)"),
        ] {
            let row = card_slice(row_with(&rows, label), label);
            assert!(
                row.contains(value),
                "{value:?} missing beside {label:?}: {row:?}"
            );
            flush_right_of(row, value);
        }
        let (cx, cy) = locate(&rows, "Current Run: jd93f2a1");
        let style = buf[(cx, cy)].style();
        assert_eq!(style.fg, Some(accent()), "current run not accent");
        assert!(
            style.add_modifier.contains(Modifier::UNDERLINED),
            "current run not underlined"
        );
    }

    // -- the input bar ---------------------------------------------------------

    #[test]
    fn the_input_bar_is_the_mocks() {
        let buf = buffer(&populated(), 161, 75);
        let rows = lines(&buf);
        let q = row_with(&rows, PLACEHOLDER);
        let inner = q.trim_start_matches(['│', ' ']);
        assert!(inner.starts_with("> "), "prompt prefix missing: {q:?}");
        assert!(q.contains("[Enter] Execute"), "execute hint missing: {q:?}");
        // The bar's border is accent, unlike the dim card borders around it.
        let (_, y) = locate(&rows, PLACEHOLDER);
        assert_eq!(
            buf[(0, y)].style().fg,
            Some(accent()),
            "input border not accent"
        );
    }

    // -- the honest empty page --------------------------------------------------

    #[test]
    fn the_empty_page_keeps_the_chrome_and_tells_the_truth() {
        let rows = draw(&empty(), 161, 75);
        row_with(&rows, "[a] Run");
        row_with(&rows, EMPTY_RUNS);
        row_with(&rows, EMPTY_WORKERS);
        row_with(&rows, EMPTY_QUEUE);
        row_with(&rows, EMPTY_GATES);
        row_with(&rows, EMPTY_EVENTS);
        let pool = row_with(&rows, "WORKER POOL");
        assert!(
            pool.contains("0/0 online"),
            "online count not a real zero: {pool:?}"
        );
        let queue = row_with(&rows, "TASK QUEUE");
        assert!(
            queue.contains("Depth: 0"),
            "depth not a real zero: {queue:?}"
        );
        // The live trio renders; every other runtime row is the honest dash.
        let binary = row_with(&rows, "Binary");
        assert!(binary.contains("emma"), "live binary missing: {binary:?}");
        let ws = row_with(&rows, "Workspace");
        assert!(
            ws.contains("~/Projects/emma"),
            "live workspace missing: {ws:?}"
        );
        for label in [
            "Runtime", "Mode", "Model", "Provider", "Started", "Uptime", "Status",
        ] {
            let row = card_slice(row_with(&rows, label), label);
            assert!(row.contains('—'), "{label} not the honest dash: {row:?}");
            flush_right_of(row, "—");
        }
        for label in [
            "CPU Usage",
            "Memory Usage",
            "Disk IO (R/W)",
            "Avg Task Latency",
        ] {
            let row = card_slice(row_with(&rows, label), label);
            flush_right_of(row, "—");
        }
        let cpu = row_with(&rows, "CPU Usage");
        assert!(!cpu.contains('█'), "empty cpu claims usage: {cpu:?}");
        let gates = row_with(&rows, "TOOL GATES / APPROVALS");
        assert!(
            gates.contains("Auto-approve: —"),
            "posture not the dash: {gates:?}"
        );
        let trace = row_with(&rows, "Trace ID");
        flush_right_of(trace, "—");
    }

    #[test]
    fn the_empty_page_makes_no_false_claims() {
        let all = draw(&empty(), 161, 75).join("\n");
        for sample in [
            "checkout-triage",
            "jd93f2a1",
            "orchestrator",
            "✓ Online",
            "0.8s",
            "tokio 1.37",
            "llama3:8b",
            "ollama",
            "2h 18m",
            "Healthy",
            "Verify alert details",
            "kubectl",
            "Auto-approve: Safe",
            "3.2 GB",
            "87% hit rate",
            "8f3c2a1b7d4e9c12",
            "Current Run:",
        ] {
            assert!(!all.contains(sample), "sample data leaked: {sample}");
        }
        let res = row_with(&draw(&empty(), 161, 75), "RESOURCES").clone();
        assert!(!res.contains("Live"), "Live claimed with no feed: {res:?}");
    }

    // -- invariants -------------------------------------------------------------

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

    // -- the keys (pure seam) ------------------------------------------------

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::from(code)
    }

    /// The same key with shift held, which is the reorder chord.
    fn shifted(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::SHIFT)
    }

    /// Five runs: more than the dashboard shows, so the cap and the ALL RUNS
    /// door both have something to do.
    fn five_runs() -> HarnessView {
        let mut v = populated();
        v.runs.push(Run {
            name: "extra-run-four".into(),
            key: "sess-mock#4".into(),
            ..Run::default()
        });
        v.runs.push(Run {
            name: "extra-run-five".into(),
            key: "sess-mock#5".into(),
            ..Run::default()
        });
        v
    }

    #[test]
    fn arrows_cycle_the_selection_through_the_visible_runs() {
        let mut v = five_runs();
        assert_eq!(v.selected_run, Some(0));
        assert_eq!(
            handle_key(&mut v, press(KeyCode::Down)),
            HarnessAction::FocusChanged
        );
        assert_eq!(v.selected_run, Some(1));
        handle_key(&mut v, press(KeyCode::Down));
        assert_eq!(v.selected_run, Some(2));
        // Wraps within the visible three, not into the hidden tail.
        handle_key(&mut v, press(KeyCode::Down));
        assert_eq!(v.selected_run, Some(0), "cycling escaped the visible boxes");
        handle_key(&mut v, press(KeyCode::Up));
        assert_eq!(v.selected_run, Some(2), "up from the top must wrap");
    }

    #[test]
    fn a_chord_and_a_release_are_never_this_pages() {
        let mut v = populated();
        let alt = KeyEvent::new(KeyCode::Char('i'), KeyModifiers::ALT);
        assert_eq!(handle_key(&mut v, alt), HarnessAction::None);
        let mut release = press(KeyCode::Down);
        release.kind = KeyEventKind::Release;
        assert_eq!(handle_key(&mut v, release), HarnessAction::None);
        assert_eq!(v.selected_run, Some(0), "a release moved the selection");
    }

    #[test]
    fn i_asks_for_the_selected_runs_full_id_and_capital_r_asks_for_a_refresh() {
        let mut v = populated();
        v.selected_run = Some(1);
        assert_eq!(
            handle_key(&mut v, press(KeyCode::Char('i'))),
            HarnessAction::Inspect("sess-mock#2".into())
        );
        assert_eq!(
            handle_key(&mut v, press(KeyCode::Char('R'))),
            HarnessAction::Refresh
        );
    }

    #[test]
    fn i_with_nothing_to_inspect_says_so_instead_of_going_dead() {
        let mut v = empty();
        assert_eq!(
            handle_key(&mut v, press(KeyCode::Char('i'))),
            HarnessAction::FocusChanged
        );
        assert_eq!(v.notice.as_deref(), Some(NOTICE_NO_RUN));
    }

    #[test]
    fn the_process_keys_ask_for_a_real_signal_against_the_selected_run() {
        use crate::runctl::{supported, Action};
        for (c, want) in [
            ('p', Action::Pause),
            ('r', Action::Resume),
            ('x', Action::Cancel),
        ] {
            let mut v = populated();
            v.selected_run = Some(1);
            let got = handle_key(&mut v, press(KeyCode::Char(c)));
            if supported(want) {
                assert_eq!(
                    got,
                    HarnessAction::Signal(want, "sess-mock#2".into()),
                    "[{c}] did not ask for its signal"
                );
                assert_eq!(v.notice, None, "[{c}] should act, not explain");
            } else {
                // The other half of the same guarantee, and the one this box
                // runs: no signal is asked for at all, and the page says why.
                assert_eq!(got, HarnessAction::FocusChanged, "[{c}] asked anyway");
                assert_eq!(
                    v.notice.as_deref(),
                    Some(
                        crate::runctl::Refusal::Unsupported(want)
                            .to_string()
                            .as_str()
                    ),
                    "[{c}] went quiet instead of answering"
                );
            }
        }
    }

    #[test]
    fn the_process_keys_with_no_run_ask_for_one_instead_of_going_dead() {
        use crate::runctl::{supported, Action};
        // Archive and Delete are run keys on every platform, so an empty page
        // owes them the same sentence. The three signals owe it only where the
        // platform has them: where it does not, "select a run" would be true
        // and useless, because selecting one would not make the key work.
        for c in ['A', 'D'] {
            let mut v = empty();
            assert_eq!(
                handle_key(&mut v, press(KeyCode::Char(c))),
                HarnessAction::FocusChanged
            );
            assert_eq!(v.notice.as_deref(), Some(NOTICE_NO_RUN), "[{c}] lied");
        }
        for (c, action) in [
            ('p', Action::Pause),
            ('r', Action::Resume),
            ('x', Action::Cancel),
        ] {
            let mut v = empty();
            assert_eq!(
                handle_key(&mut v, press(KeyCode::Char(c))),
                HarnessAction::FocusChanged
            );
            let want = if supported(action) {
                NOTICE_NO_RUN.to_string()
            } else {
                crate::runctl::Refusal::Unsupported(action).to_string()
            };
            assert_eq!(v.notice.as_deref(), Some(want.as_str()), "[{c}] lied");
        }
    }

    /// Ruling 4, on the box this is first tested on: the three controls this
    /// platform does not have answer in words, name the platform and the
    /// operation, and produce no [`HarnessAction::Signal`] for the shell to
    /// carry out.
    #[test]
    #[cfg(not(unix))]
    fn the_control_keys_refuse_in_words_here_and_ask_for_no_signal() {
        use crate::runctl::Action;
        // The verb each refusal must use. Spelled here rather than read off
        // `Action`, so a reworded sentence that stopped naming its operation
        // turns this red instead of agreeing with itself.
        for (c, action, verb) in [
            ('p', Action::Pause, "pausing"),
            ('r', Action::Resume, "resuming"),
            ('x', Action::Cancel, "cancelling"),
        ] {
            let mut v = populated();
            v.selected_run = Some(1);
            let got = handle_key(&mut v, press(KeyCode::Char(c)));
            assert_eq!(
                got,
                HarnessAction::FocusChanged,
                "[{c}] must not reach the shell at all"
            );
            assert!(
                !matches!(got, HarnessAction::Signal(..)),
                "[{c}] asked for a signal on a platform that cannot send one"
            );
            let notice = v.notice.clone().unwrap_or_default();
            assert_eq!(
                notice,
                crate::runctl::Refusal::Unsupported(action).to_string(),
                "[{c}] wrote its own words instead of the refusal's"
            );
            assert!(
                notice.contains("Windows"),
                "[{c}]'s refusal does not name the platform: {notice:?}"
            );
            assert!(
                notice.contains(verb),
                "[{c}]'s refusal does not name the operation ({verb}): {notice:?}"
            );
        }
    }

    /// The other half: what a reader sees before pressing anything.
    #[test]
    fn the_action_bar_marks_a_control_this_platform_does_not_have() {
        use crate::runctl::{supported, Action};
        // 161x40 is the shape the real page was certified at, not a width
        // picked to make the sentence fit.
        let rows = draw(&populated(), 161, 40);
        let head = row_with(&rows, SUBTITLE);
        let bar = row_with(&rows, "[i] Inspect");
        match no_controls_note() {
            None => {
                assert!(supported(Action::Pause), "a note is owed but none is drawn");
                assert!(
                    !head.contains("not offered"),
                    "the page disowns controls it has: {head:?}"
                );
            }
            Some(note) => {
                assert!(
                    !supported(Action::Pause),
                    "a note is drawn but none is owed"
                );
                assert!(
                    note.contains(std::env::consts::OS),
                    "the note does not name the platform: {note:?}"
                );
                for k in ["[p", "r", "x]"] {
                    assert!(note.contains(k), "the note does not name {k}: {note:?}");
                }
                assert!(
                    head.contains(&note),
                    "the note never reached the page: {head:?}"
                );
                // And the chips it is about are drawn, but not as live
                // controls: `[i] Inspect` keeps the accent, `[p] Pause` does
                // not. Read off the buffer, because "drawn as if it worked"
                // is a question about colour and not about text.
                assert!(bar.contains("[p] Pause"), "the chip vanished: {bar:?}");
                let buf = buffer(&populated(), 161, 40);
                let rows = lines(&buf);
                let (lx, ly) = locate(&rows, "[i] Inspect");
                assert_eq!(
                    buf[(lx, ly)].style().bg,
                    Some(accent()),
                    "a live chip lost its accent, so the comparison proves nothing"
                );
                for (key, _) in CONTROL_KEYS {
                    let (x, y) = locate(&rows, &format!("[{key}]"));
                    assert_ne!(
                        buf[(x, y)].style().bg,
                        Some(accent()),
                        "[{key}] is drawn as a live control the platform refuses"
                    );
                }
            }
        }
        // And `[?]` advertises the three chords exactly where they work. The
        // Windows spelling still says the words, in a clause that says to
        // press one for the reason, so the match is on the advertisement.
        assert_eq!(
            HELP_DASH.contains("[p]/[r]/[x] pause/resume/cancel"),
            supported(Action::Pause),
            "the help text and the platform disagree: {HELP_DASH:?}"
        );
    }

    #[test]
    fn logs_still_says_there_is_no_logs_view() {
        let mut v = populated();
        handle_key(&mut v, press(KeyCode::Char('l')));
        assert_eq!(v.notice.as_deref(), Some(NOTICE_LOGS));
    }

    #[test]
    fn a_run_key_needs_a_goal_in_the_command_bar() {
        let mut v = populated();
        v.command.clear();
        for c in ['a', '+'] {
            assert_eq!(
                handle_key(&mut v, press(KeyCode::Char(c))),
                HarnessAction::FocusChanged
            );
            assert_eq!(v.notice.as_deref(), Some(NOTICE_NO_GOAL), "[{c}] lied");
        }
        v.command = "  fix the parser  ".into();
        assert_eq!(
            handle_key(&mut v, press(KeyCode::Char('a'))),
            HarnessAction::Launch("fix the parser".into())
        );
    }

    #[test]
    fn archive_names_the_selected_run_and_delete_asks_twice() {
        let mut v = populated();
        v.selected_run = Some(1);
        assert_eq!(
            handle_key(&mut v, press(KeyCode::Char('A'))),
            HarnessAction::Archive("sess-mock#2".into())
        );

        // Once is a question, not a deletion.
        assert_eq!(
            handle_key(&mut v, press(KeyCode::Char('D'))),
            HarnessAction::FocusChanged
        );
        assert_eq!(v.notice.as_deref(), Some(CONFIRM_DELETE));
        assert_eq!(
            handle_key(&mut v, press(KeyCode::Char('D'))),
            HarnessAction::Delete("sess-mock#2".into())
        );
        assert_eq!(v.armed, None, "the confirmation must not stay armed");
    }

    #[test]
    fn any_other_key_cancels_an_armed_delete_and_is_not_also_acted_on() {
        let mut v = populated();
        handle_key(&mut v, press(KeyCode::Char('D')));
        // `R` is Refresh everywhere else on this page. Here it only cancels.
        assert_eq!(
            handle_key(&mut v, press(KeyCode::Char('R'))),
            HarnessAction::FocusChanged
        );
        assert_eq!(v.armed, None);
        assert_eq!(v.notice, None);
        // And a second `D` after the cancel is a fresh first press.
        assert_eq!(
            handle_key(&mut v, press(KeyCode::Char('D'))),
            HarnessAction::FocusChanged
        );
        assert_eq!(v.notice.as_deref(), Some(CONFIRM_DELETE));
    }

    #[test]
    fn clear_done_asks_twice_too() {
        let mut v = populated();
        assert_eq!(
            handle_key(&mut v, press(KeyCode::Char('c'))),
            HarnessAction::FocusChanged
        );
        assert_eq!(v.notice.as_deref(), Some(CONFIRM_CLEAR));
        assert_eq!(
            handle_key(&mut v, press(KeyCode::Char('c'))),
            HarnessAction::TaskClearDone
        );
    }

    #[test]
    fn a_new_task_takes_the_command_bar_the_way_a_new_run_does() {
        let mut v = populated();
        v.command.clear();
        assert_eq!(
            handle_key(&mut v, press(KeyCode::Char('n'))),
            HarnessAction::FocusChanged
        );
        assert!(v
            .notice
            .as_deref()
            .is_some_and(|n| n.contains("command bar")));
        v.command = "write the migration".into();
        assert_eq!(
            handle_key(&mut v, press(KeyCode::Char('n'))),
            HarnessAction::TaskNew("write the migration".into())
        );
    }

    #[test]
    fn tab_moves_the_arrows_between_the_two_cards_that_have_a_selection() {
        let mut v = populated();
        assert_eq!(v.focus, Focus::Runs);
        handle_key(&mut v, press(KeyCode::Down));
        assert_eq!(v.selected_run, Some(1), "runs have the arrows first");

        handle_key(&mut v, press(KeyCode::Tab));
        assert_eq!(v.focus, Focus::Queue);
        let before = v.selected_task;
        handle_key(&mut v, press(KeyCode::Down));
        assert_ne!(v.selected_task, before, "the queue now has the arrows");
        assert_eq!(v.selected_run, Some(1), "and the run selection stayed put");

        handle_key(&mut v, press(KeyCode::Tab));
        assert_eq!(v.focus, Focus::Runs);
    }

    #[test]
    fn shifted_arrows_reorder_only_the_focused_queue() {
        let mut v = populated();
        // Focus on the runs: a shifted arrow is just an arrow.
        assert_eq!(
            handle_key(&mut v, shifted(KeyCode::Down)),
            HarnessAction::FocusChanged
        );
        handle_key(&mut v, press(KeyCode::Tab));
        assert_eq!(
            handle_key(&mut v, shifted(KeyCode::Up)),
            HarnessAction::TaskMove(true)
        );
        assert_eq!(
            handle_key(&mut v, shifted(KeyCode::Down)),
            HarnessAction::TaskMove(false)
        );
    }

    #[test]
    fn the_policy_keys_ask_for_a_next_run_policy_and_t_only_reports() {
        use crate::runctl::Policy;
        let mut v = populated();
        assert_eq!(
            handle_key(&mut v, press(KeyCode::Char('s'))),
            HarnessAction::Policy(Policy::Safe)
        );
        assert_eq!(
            handle_key(&mut v, press(KeyCode::Char('m'))),
            HarnessAction::Policy(Policy::Manual)
        );
        assert_eq!(
            handle_key(&mut v, press(KeyCode::Char('d'))),
            HarnessAction::Policy(Policy::DenyAll)
        );
        assert_eq!(
            handle_key(&mut v, press(KeyCode::Char('t'))),
            HarnessAction::PolicyShow
        );
    }

    #[test]
    fn the_worker_key_names_the_missing_backend_rather_than_inventing_one() {
        // The one control on this page with nothing behind it, and the only
        // honest notice left.
        let mut v = populated();
        assert_eq!(
            handle_key(&mut v, press(KeyCode::Char('w'))),
            HarnessAction::FocusChanged
        );
        assert_eq!(v.notice.as_deref(), Some(NOTICE_WORKERS));
        assert!(
            NOTICE_WORKERS.contains("delegate.rs"),
            "the notice must name the fact"
        );
    }

    /// The gates card names whichever of gate and posture is deciding.
    #[test]
    fn the_gates_card_names_plan_mode_rather_than_the_gate_it_started_in() {
        let mut v = populated();
        v.auto_approve = "ASK".into();
        v.mode_label = crate::approval::Mode::Plan.label().to_string();
        let rows = draw(&v, 220, 75);
        let header = row_with(&rows, "TOOL GATES");
        assert!(
            header.contains(GATE_PLAN),
            "the card kept the started gate under plan mode: {header:?}"
        );
        assert!(
            !header.contains("Auto-approve: ASK"),
            "the card still claims writes are asked about: {header:?}"
        );
        // And every other posture is left alone.
        v.mode_label = crate::approval::Mode::Assist.label().to_string();
        let rows = draw(&v, 220, 75);
        assert!(row_with(&rows, "TOOL GATES").contains("Auto-approve: ASK"));
    }

    #[test]
    fn help_toggles_and_esc_clears_the_notice() {
        let mut v = populated();
        assert_eq!(
            handle_key(&mut v, press(KeyCode::Char('?'))),
            HarnessAction::Help
        );
        assert_eq!(v.notice.as_deref(), Some(HELP_DASH));
        handle_key(&mut v, press(KeyCode::Char('?')));
        assert_eq!(v.notice, None, "[?] must toggle off");
        handle_key(&mut v, press(KeyCode::Char('l')));
        handle_key(&mut v, press(KeyCode::Esc));
        assert_eq!(v.notice, None, "Esc must clear the notice");
    }

    /// `[g]` on a repo with runs asks the shell to read that run's records;
    /// the graph is one run's trace, so the key cannot mount it alone.
    #[test]
    fn g_asks_for_the_selected_runs_graph() {
        let mut v = populated();
        v.selected_run = Some(1);
        let want = v.runs[1].key.clone();
        assert_eq!(
            handle_key(&mut v, press(KeyCode::Char('g'))),
            HarnessAction::Graph(want)
        );
        assert_eq!(v.mode, PageMode::Dashboard, "the shell mounts, not the key");
    }

    /// With no run on disk there is nothing to read, and the page mounts its
    /// own no-run state rather than going dead.
    #[test]
    fn g_with_no_runs_mounts_the_honest_empty_graph_and_b_returns() {
        let mut v = populated();
        v.runs.clear();
        v.selected_run = None;
        assert_eq!(
            handle_key(&mut v, press(KeyCode::Char('g'))),
            HarnessAction::FocusChanged
        );
        assert_eq!(v.mode, PageMode::Graph);
        let rows = draw(&v, 161, 75);
        row_with(&rows, super::super::rungraph::EMPTY_GRAPH);
        handle_key(&mut v, press(KeyCode::Char('b')));
        assert_eq!(v.mode, PageMode::Dashboard);
        assert!(
            v.graph.is_none() && v.graph_id.is_none(),
            "the graph view must unmount"
        );
    }

    #[test]
    fn the_inspect_mode_renders_the_mounted_view_and_b_unmounts_it() {
        let mut v = populated();
        v.mode = PageMode::Inspect;
        v.inspect = Some(super::super::inspect::InspectView {
            version: "v0.6.3".into(),
            run: None,
        });
        v.inspect_id = Some("sess-mock#1".into());
        let rows = draw(&v, 161, 75);
        row_with(&rows, "Inspect Run");
        handle_key(&mut v, press(KeyCode::Esc));
        assert_eq!(v.mode, PageMode::Dashboard);
        assert!(v.inspect.is_none() && v.inspect_id.is_none());
    }

    #[test]
    fn inspect_arrows_cycle_the_tool_call_selection() {
        let mut v = populated();
        v.mode = PageMode::Inspect;
        let run = super::super::inspect::RunView {
            tools: vec![
                super::super::inspect::ToolCall {
                    tool: "Read".into(),
                    request: "read x".into(),
                    status: super::super::inspect::ToolStatus::Allowed,
                    time: "0.1s".into(),
                },
                super::super::inspect::ToolCall {
                    tool: "Bash".into(),
                    request: "ls".into(),
                    status: super::super::inspect::ToolStatus::Allowed,
                    time: "0.2s".into(),
                },
            ],
            selected_tool: Some(0),
            ..super::super::inspect::RunView::default()
        };
        v.inspect = Some(super::super::inspect::InspectView {
            version: String::new(),
            run: Some(run),
        });
        handle_key(&mut v, press(KeyCode::Down));
        let sel = v
            .inspect
            .as_ref()
            .unwrap()
            .run
            .as_ref()
            .unwrap()
            .selected_tool;
        assert_eq!(sel, Some(1));
        handle_key(&mut v, press(KeyCode::Down));
        let sel = v
            .inspect
            .as_ref()
            .unwrap()
            .run
            .as_ref()
            .unwrap()
            .selected_tool;
        assert_eq!(sel, Some(0), "the tool selection must wrap");
    }

    #[test]
    fn v_opens_all_runs_where_arrows_reach_the_hidden_tail() {
        let mut v = five_runs();
        handle_key(&mut v, press(KeyCode::Char('v')));
        assert_eq!(v.mode, PageMode::AllRuns);
        for _ in 0..4 {
            handle_key(&mut v, press(KeyCode::Down));
        }
        assert_eq!(v.all_selected, 4, "ALL RUNS must reach every run");
        assert_eq!(
            handle_key(&mut v, press(KeyCode::Enter)),
            HarnessAction::Inspect("sess-mock#5".into())
        );
        handle_key(&mut v, press(KeyCode::Char('b')));
        assert_eq!(v.mode, PageMode::Dashboard);
    }

    #[test]
    fn the_all_runs_page_lists_what_the_dashboard_hides() {
        let mut v = five_runs();
        v.mode = PageMode::AllRuns;
        let rows = draw(&v, 161, 75);
        row_with(&rows, "extra-run-five");
        row_with(&rows, "ALL RUNS");
    }

    // -- the mouse (rects at paint, pure hit-test) ---------------------------

    fn hits_at(v: &HarnessView, w: u16, h: u16) -> (Buffer, Hits) {
        let area = Rect::new(0, 0, w, h);
        let mut buf = Buffer::empty(area);
        let hits = render_hits(area, &mut buf, v, &skin());
        (buf, hits)
    }

    #[test]
    fn the_paint_reports_the_run_boxes_and_the_action_chips() {
        let (_, hits) = hits_at(&populated(), 161, 75);
        assert_eq!(hits.runs.len(), 3, "one rect per painted run box");
        // The action bar's chips, then every card footer's: a footer key is a
        // control like any other and clicking it fires the same dispatch.
        assert!(
            hits.chips.len() > ACTIONS.len(),
            "the card footers report no chips"
        );
        assert_eq!(hits.chips[0].1, 'a');
        for key in ['+', 'A', 'D', 'n', 'c', 't', 's', 'm', 'd', 'w'] {
            let (r, _) = hits
                .chips
                .iter()
                .find(|(_, k)| *k == key)
                .unwrap_or_else(|| panic!("no rect for the [{key}] footer chip"));
            assert_eq!(hit(&hits, r.x, r.y), Some(Hit::Chip(key)));
        }
        let r = hits.runs[1];
        assert_eq!(
            hit(&hits, r.x + 1, r.y + 1),
            Some(Hit::Run(1)),
            "a press inside box 1 must select it"
        );
        let (chip_rect, _) = hits
            .chips
            .iter()
            .find(|(_, k)| *k == 'R')
            .expect("the [R] chip");
        assert_eq!(hit(&hits, chip_rect.x, chip_rect.y), Some(Hit::Chip('R')));
        assert_eq!(hit(&hits, 0, 0), None, "a miss is a miss");
    }

    #[test]
    fn the_hidden_tail_records_no_rect() {
        let (_, hits) = hits_at(&five_runs(), 161, 75);
        assert_eq!(
            hits.runs.len(),
            VISIBLE_RUNS,
            "a rect for an unpainted box is a lie"
        );
    }

    #[test]
    fn the_action_chips_wear_the_chip_dress() {
        let (buf, _) = hits_at(&populated(), 161, 75);
        let rows = lines(&buf);
        let (x, y) = locate(&rows, "[a] Run");
        assert_eq!(
            buf[(x, y)].style().bg,
            Some(accent()),
            "the bracketed key is a colored chip, not bare accent text"
        );
        // The label beside it stays dim text on the page ground.
        assert_ne!(
            buf[(x + 4, y)].style().bg,
            Some(accent()),
            "the label must not join the chip"
        );
    }

    // -- the fidelity fixes: cap, anchor, compression ------------------------

    #[test]
    fn the_dashboard_caps_the_run_boxes_at_the_mocks_three() {
        let rows = draw(&five_runs(), 161, 75);
        row_with(&rows, "rollback-procedure");
        assert!(
            !rows.iter().any(|r| r.contains("extra-run-five")),
            "a fourth box leaked past the cap"
        );
        let more = row_with(&rows, "2 more");
        assert!(
            more.contains("…"),
            "the hidden tail must be counted honestly: {more:?}"
        );
    }

    #[test]
    fn the_input_bar_is_anchored_to_the_bottom_at_every_height() {
        for h in [75u16, 50, 45, 40] {
            let rows = draw(&populated(), 161, h);
            let (_, y) = locate(&rows, PLACEHOLDER);
            assert_eq!(
                y,
                h - 2,
                "at 161x{h} the input row sits at {y}, not anchored"
            );
        }
    }

    #[test]
    fn a_short_window_compresses_every_card_instead_of_dropping_the_bottom() {
        for (w, h) in [(161u16, 45u16), (141, 45), (161, 40)] {
            let rows = draw(&populated(), w, h);
            for header in [
                "ACTIVE RUNS",
                "WORKER POOL",
                "RUNTIME STATUS",
                "TASK QUEUE",
                "TOOL GATES / APPROVALS",
                "RESOURCES",
                "EVENT LOG",
                "TRACE / CONTEXT",
            ] {
                assert!(
                    rows.iter().any(|r| r.contains(header)),
                    "{header} vanished at {w}x{h}"
                );
            }
        }
    }

    #[test]
    fn the_notice_renders_above_the_input_bar_until_dismissed() {
        let mut v = populated();
        v.notice = Some(NOTICE_WORKERS.to_string());
        let rows = draw(&v, 161, 75);
        let (_, ny) = locate(&rows, NOTICE_WORKERS);
        let (_, qy) = locate(&rows, PLACEHOLDER);
        assert!(
            ny < qy && qy - ny <= 3,
            "the notice must sit just above the bar"
        );
    }

    #[test]
    fn a_run_with_no_recorded_ceiling_claims_no_progress() {
        // The loop records a ceiling nowhere for a live goal (RunRow docs):
        // a gauge at 0/0 (0%) would be a bar moving at a rate nobody
        // measured. The honest dash is the idiom.
        let mut v = populated();
        v.runs[0].done = 0;
        v.runs[0].total = 0;
        let rows = draw(&v, 161, 75);
        assert!(
            !rows.iter().any(|r| r.contains("0/0 (0%)")),
            "a measured-looking zero leaked from an unmeasured run"
        );
        let (_, name_y) = locate(&rows, "checkout-triage");
        let gauge_row = &rows[usize::from(name_y) + 2];
        assert!(
            gauge_row.contains('–'),
            "the dash placeholder is missing: {gauge_row:?}"
        );
    }

    #[test]
    fn a_floor_height_table_card_shows_the_count_and_no_footer_collision() {
        // band_heights' floor hands a card 4 rows: borders, header, one
        // content row. That row must be the honest count — not the footer
        // painted over the column header with residue left behind.
        let area = Rect::new(0, 0, 70, 4);
        let mut buf = Buffer::empty(area);
        render_gates(
            area,
            &mut buf,
            &populated(),
            &skin(),
            &page_glyphs(&skin()),
            &mut Vec::new(),
        );
        let rows = lines(&buf);
        let content = &rows[2];
        assert!(
            content.contains("5 more"),
            "the hidden rows are uncounted: {content:?}"
        );
        assert!(
            !content.contains("[t] Show"),
            "the footer stole the one honest row: {content:?}"
        );
        // One row taller: the column header returns, the count stays.
        let area = Rect::new(0, 0, 70, 5);
        let mut buf = Buffer::empty(area);
        render_gates(
            area,
            &mut buf,
            &populated(),
            &skin(),
            &page_glyphs(&skin()),
            &mut Vec::new(),
        );
        let rows = lines(&buf);
        assert!(
            rows[2].contains("TOOL"),
            "the column header vanished: {:?}",
            rows[2]
        );
        assert!(
            rows[3].contains("5 more"),
            "the count vanished: {:?}",
            rows[3]
        );
    }

    /// Eyeball dump: `cargo test -p emma the_populated_harness -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn the_populated_harness_at_161x75_for_eyeballing() {
        for row in draw(&populated(), 161, 75) {
            println!("{row}");
        }
    }

    /// A live-shaped view: the empties the real feeds leave, plus a few
    /// real-shaped runs and events — what the owner's terminal actually
    /// shows today. Eyeball dump:
    /// `cargo test -p emma the_live_shaped_harness -- --ignored --nocapture`.
    fn live_shaped() -> HarnessView {
        let mut v = empty();
        v.runs = vec![
            Run {
                name: "Fix the flaky retry test in ollama.rs".into(),
                state: RunState::Running,
                id: "00000-1#2".into(),
                key: "sess-1756000000000-1#2".into(),
                stamp: "14:02:11".into(),
                ..Run::default()
            },
            Run {
                name: "Add a clamp helper".into(),
                state: RunState::Completed,
                id: "00000-1#1".into(),
                key: "sess-1756000000000-1#1".into(),
                stamp: "13:40:02".into(),
                ..Run::default()
            },
        ];
        v.selected_run = Some(0);
        v.auto_approve = "ASSIST".into();
        v.current_run = "00000-1#2".into();
        v.events = vec![
            Event {
                time: "14:02:11".into(),
                level: LogLevel::Info,
                text: "goal: Fix the flaky retry test in ollama.rs".into(),
            },
            Event {
                time: "14:02:14".into(),
                level: LogLevel::Info,
                text: "Read src/ollama.rs".into(),
            },
            Event {
                time: "14:02:19".into(),
                level: LogLevel::Warn,
                text: "denied by user: Write src/ollama.rs".into(),
            },
        ];
        v
    }

    // -- the Run Graph's viewport ---------------------------------------------

    /// A chain long enough that no terminal shows all of it.
    fn graph_page() -> HarnessView {
        use super::super::rungraph as rg;
        let mut v = populated();
        v.mode = PageMode::Graph;
        let nodes: Vec<rg::Node> = (0..8)
            .map(|i| rg::Node {
                id: format!("n{i}"),
                name: format!("step {i}"),
                kind: rg::Kind::Agent,
                status: rg::Status::Completed,
                time: "1.0s".into(),
                parents: if i == 0 { Vec::new() } else { vec![i - 1] },
            })
            .collect();
        v.graph = Some(rg::GraphView {
            version: "v0.6.3".into(),
            nodes,
            selected: Some(0),
            canvas_rows: 20,
            ..rg::GraphView::default()
        });
        v.graph_id = Some("sess-mock#1".into());
        v
    }

    fn graph_of(v: &HarnessView) -> &super::super::rungraph::GraphView {
        v.graph.as_ref().expect("the graph must be mounted")
    }

    /// The owner's report: navigate changed the inspector and left the
    /// canvas still. The key path must move the viewport with the cursor.
    #[test]
    fn arrow_keys_drag_the_dag_viewport_with_the_selection() {
        let mut v = graph_page();
        for _ in 0..5 {
            handle_key(&mut v, press(KeyCode::Down));
        }
        let g = graph_of(&v);
        assert_eq!(g.selected, Some(5));
        assert!(
            g.scroll > 0,
            "the selection went below the fold and the canvas stayed put"
        );
    }

    #[test]
    fn j_and_k_pan_the_canvas_without_moving_the_selection() {
        let mut v = graph_page();
        handle_key(&mut v, press(KeyCode::Char('j')));
        handle_key(&mut v, press(KeyCode::Char('j')));
        let g = graph_of(&v);
        assert_eq!(g.scroll, 2);
        assert_eq!(g.selected, Some(0), "a pan is not a selection");
        handle_key(&mut v, press(KeyCode::Char('k')));
        assert_eq!(graph_of(&v).scroll, 1);
    }

    #[test]
    fn page_keys_move_a_canvas_at_a_time_and_home_end_reach_both_edges() {
        let mut v = graph_page();
        handle_key(&mut v, press(KeyCode::PageDown));
        assert_eq!(
            graph_of(&v).scroll,
            20,
            "PgDn moved something other than one canvas"
        );
        handle_key(&mut v, press(KeyCode::PageUp));
        assert_eq!(graph_of(&v).scroll, 0);
        handle_key(&mut v, press(KeyCode::End));
        let g = graph_of(&v);
        assert_eq!(g.scroll, super::super::rungraph::max_scroll(g));
        handle_key(&mut v, press(KeyCode::Home));
        assert_eq!(graph_of(&v).scroll, 0);
    }

    /// The palette owns the keyboard while it is open, so `j` is a letter in
    /// a command and not a pan, and Esc gives the page back its keys.
    #[test]
    fn the_command_palette_keeps_j_and_k_while_it_has_the_keyboard() {
        let mut v = graph_page();
        handle_key(&mut v, press(KeyCode::Char(':')));
        handle_key(&mut v, press(KeyCode::Char('j')));
        handle_key(&mut v, press(KeyCode::Char('k')));
        let g = graph_of(&v);
        assert_eq!(g.query, "jk", "the palette lost the letters to the pan");
        assert_eq!(g.scroll, 0, "the canvas moved while someone was typing");
        handle_key(&mut v, press(KeyCode::Esc));
        assert!(!graph_of(&v).palette);
        handle_key(&mut v, press(KeyCode::Char('j')));
        assert_eq!(
            graph_of(&v).scroll,
            1,
            "Esc did not give the pan its key back"
        );
    }

    /// `[b]` still leaves, whatever the viewport is doing.
    #[test]
    fn b_still_leaves_a_scrolled_graph() {
        let mut v = graph_page();
        handle_key(&mut v, press(KeyCode::End));
        handle_key(&mut v, press(KeyCode::Char('b')));
        assert_eq!(v.mode, PageMode::Dashboard);
        assert!(v.graph.is_none());
    }

    /// The paint reports the canvas height, and only on the graph page.
    #[test]
    fn render_hits_reports_the_dag_canvas_height() {
        let v = graph_page();
        let area = Rect::new(0, 0, 161, 75);
        let mut buf = Buffer::empty(area);
        let hits = render_hits(area, &mut buf, &v, &skin());
        assert!(
            hits.graph_canvas_rows > 0,
            "the graph page reported no canvas"
        );
        let mut dash = populated();
        dash.mode = PageMode::Dashboard;
        let mut buf = Buffer::empty(area);
        assert_eq!(
            render_hits(area, &mut buf, &dash, &skin()).graph_canvas_rows,
            0
        );
    }

    #[test]
    #[ignore]
    fn the_live_shaped_harness_at_161x75_for_eyeballing() {
        for row in draw(&live_shaped(), 161, 75) {
            println!("{row}");
        }
    }

    /// Certification, not a guarantee: draw the page over the real session
    /// directory on the machine running it.
    ///
    /// `#[ignore]`d because it reads a store this repository does not own and
    /// a machine that has never run Emma has nothing to show — a test whose
    /// result depends on somebody's home directory is not a test. It is here
    /// because a cell buffer full of fixtures agrees with its author: the
    /// mapping below is the one `app::harness_view_from` uses, aimed at the
    /// real feed, and what it prints is what a reader would see. Nothing is
    /// signalled, archived or deleted; `harness_state::runs` only reads.
    #[test]
    #[ignore]
    fn the_real_session_directory_for_certification() {
        use crate::harness_state as hs;
        let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))
        else {
            println!("no home directory on this machine; nothing to certify against");
            return;
        };
        let dir = std::path::Path::new(&home).join(".emma/sessions");
        let cwd = std::env::current_dir().unwrap();
        // `--lib` runs from the crate directory; the runs are recorded against
        // the workspace root, which is two levels up.
        let cwd = cwd.parent().and_then(|p| p.parent()).unwrap_or(&cwd);
        let cwd = cwd.display().to_string();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let feed = hs::runs(&dir, now).expect("the real session directory could not be read");
        let mine: Vec<&hs::RunRow> = feed
            .runs
            .iter()
            .filter(|r| r.cwd.as_deref() == Some(cwd.as_str()))
            .collect();
        println!(
            "{} session files, {} runs, {} in this repository",
            std::fs::read_dir(&dir).map(|d| d.count()).unwrap_or(0),
            feed.runs.len(),
            mine.len()
        );
        let mut v = HarnessView {
            version: concat!("v", env!("CARGO_PKG_VERSION")).to_string(),
            ..HarnessView::default()
        };
        v.runs = mine
            .iter()
            .map(|r| Run {
                name: r.name.clone(),
                key: r.id.clone(),
                state: match r.status {
                    hs::RunStatus::Running => RunState::Running,
                    hs::RunStatus::Completed => RunState::Completed,
                    hs::RunStatus::Failed => RunState::Failed,
                    hs::RunStatus::Stalled => RunState::Paused,
                },
                id: r
                    .id
                    .chars()
                    .rev()
                    .take(8)
                    .collect::<String>()
                    .chars()
                    .rev()
                    .collect(),
                stamp: String::new(),
                done: r.progress.as_ref().map(|p| p.done as u32).unwrap_or(0),
                total: r.progress.as_ref().map(|p| p.total as u32).unwrap_or(0),
            })
            .collect();
        if !v.runs.is_empty() {
            v.selected_run = Some(0);
        }
        for row in draw(&v, 161, 40) {
            println!("{row}");
        }
    }

    #[test]
    #[ignore]
    fn the_populated_harness_at_141x45_for_eyeballing() {
        for row in draw(&populated(), 141, 45) {
            println!("{row}");
        }
    }
}
