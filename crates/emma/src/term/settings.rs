//! The Settings screen: the owner's mock, cell for cell — and live.
//!
//! The mock is the acceptance criterion (the Settings TUI design): a
//! title block with the version in the top-right corner, eight numbered cards
//! in a two-by-four grid (a ninth, LANGUAGE SERVERS, was added after the mock
//! and takes the left of a fifth row), every card a bordered box of `Label … value` rows
//! with the value right-aligned in the accent, a thin divider where the mock
//! draws one, and a dim one-line description at the bottom of the cards that
//! carry one.
//!
//! **The mock's sample values are gone, and that reverses this module's own
//! policy.** It used to read *"a missing row is a deviation, so a field with no
//! backing state renders the mock's sample value rather than being dropped"*,
//! and the row kept its accent — so `Max Context Tokens 8192` and
//! `Shell  Ask ›` were painted identically to a live `Model`, with the caveat
//! reachable only by pressing Enter on the row. Owner ruling, 2026-08-27
//! (the settings-wiring design): every row ends in exactly one of three
//! states, and each is visually distinct.
//!
//! 1. **Live and editable** — [`Value::Cycler`] or [`Value::Button`], accent,
//!    and a [`RowKind`] that crosses [`SettingsAction`] to a real write.
//! 2. **Live and read-only** — [`Value::Plain`], accent, **no** edit
//!    affordance, and a notice naming where it *is* changed.
//! 3. **Absent** — [`Value::Absent`], drawn dim rather than in the accent, and
//!    reading `n/a` or `not set`. Never a plausible placeholder: a placeholder
//!    is indistinguishable from data once the mockup is out of the room.
//!
//! The dim/accent split is the load-bearing half. A reader who presses nothing
//! must still be able to tell which figures on the page are this run's, and
//! colour is the only channel a `Label … value` row has left.
//!
//! Interaction (settings-live, 2026-08-26) is the memory/harness pattern, two
//! pure halves. [`handle_key`] mutates focus and notices and returns a
//! [`SettingsAction`] for everything that needs disk or network — the shell
//! half is `App::settings_key`, which owns `settings.json`, the palette
//! registry and the ping. The mouse is the harness's: [`render_hits`] records
//! every control's rect from the same arithmetic that painted it, [`hit`] is a
//! pure test against that record, and a click dispatches through the same key
//! path, so the two can never disagree about what a control does.
//!
//! **Every row answers, and every answer is honest.** The rows with backing —
//! Theme, Enable Memory, Prune History, Save, Export, Reset, and Test
//! Connection against a local Ollama — really act and write real files. The
//! rows that read a fact this run resolved (model, cwd, provider, the context
//! cap, the permission rules) show it and draw nothing to press. The rows with
//! no backing at all say `n/a` in the dim role and answer with a notice that
//! names the mechanism that actually exists, never "not implemented" alone.
//! Which row is which is [`RowKind`], one entry per Kv row, so a test can
//! assert the class table without parsing a buffer.
//!
//! **Card 6 is not the mock's card 6, and the difference is a safety claim.**
//! The mock lists six rows — Shell, Code, File Browser, Search, Memory, Data
//! Explorer — with `Ask`/`Allow` beside each. Those are `usertools::Tool`
//! entries: programs *the person* launches with an `Alt` chord, which have no
//! permission model in this tree and never had one. The thing Emma really
//! gates is the **model's** tool calls, against the rules in
//! `<harness>/settings.local.json` and the project's spine file. So the card
//! shows those rules — the real ones, in precedence order, read-only — and six
//! invented verdicts about six launcher rows are not on the page. A fabricated
//! `Allow` on a permission screen is not a cosmetic defect.

use ratatui::buffer::Buffer;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Widget};

use super::palette::Role;
use super::render::{cols, corner_row, fit, Skin, ASCII};
use super::termfont;

// region: State
// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

/// What the screen shows. The live fields; everything else is the mock's
/// sample data, held as constants below so a test and the renderer name the
/// same strings.
#[derive(Debug, Clone, Default)]
pub struct SettingsView {
    /// The running binary's version, `v`-prefixed — the mock's top-right corner.
    pub version: String,
    /// The live model, from `Status.model`.
    pub model: String,
    /// The live working directory, from `Status.cwd`.
    pub cwd: String,
    /// The provider **this session is bound to**, by its registry name
    /// (`anthropic`, `ollama`). Set when the screen opens and never changed
    /// from here: see [`SettingsView::provider_saved`].
    pub provider: String,
    /// The provider stored in settings.json, which is what the next run will
    /// boot with. Cycling the row writes this one.
    ///
    /// Two fields rather than one because they are two facts and the row shows
    /// both whenever they disagree. Rebinding a live client mid-session is
    /// `main.rs` machinery and this screen does not own half of it, so the
    /// alternative to a second field is a row that reads as a switch and
    /// silently is not.
    pub provider_saved: String,
    /// Every provider this build can run, and whether a key for it is on
    /// disk. Names and a yes/no only: the key itself never reaches this layer,
    /// so there is nothing here that could be printed by accident.
    ///
    /// Empty means the screen has not read it, which the row says rather than
    /// claiming nothing is configured.
    pub provider_keys: Vec<(String, KeyPresence)>,
    /// The selected theme, by the name `/theme` takes. Live: ←/→ on the Theme
    /// row steps through [`SettingsView::themes`] exactly as `/theme <name>`
    /// does.
    pub theme: String,
    /// Every name the Theme row may step to, in `theme::names` order — the
    /// built-in first, then each themes directory, no duplicates.
    ///
    /// **A list rather than an array, and that is the shape of this tree's
    /// theme system.** The screen arrived expecting a compiled-in
    /// `palette::THEMES` it could index; here a theme is a JSON file under
    /// `~/.emma/themes` or the harness root, so what is selectable is a fact
    /// about a disk read at the moment the screen opens. Empty is impossible —
    /// the built-in is always in it — so the cycler never divides by zero.
    pub themes: Vec<String>,
    /// The `memory_capture` setting, with its absent-means-on default already
    /// resolved by the caller (`Settings::capture_raws`).
    pub memory_on: bool,
    /// The `prune_history` setting, absent-means-**off** already resolved by
    /// the caller. The opposite default to `memory_on`, and deliberately so —
    /// see `crate::settings::Settings::prune_history`.
    pub prune_on: bool,
    /// The permission rules in force for this project, in the order they are
    /// consulted: deny, then ask, then allow. Read once when the screen opens.
    ///
    /// Empty and [`SettingsView::perms_read`] true means a project with no
    /// rules at all, which is a different fact from "nobody looked" and is
    /// drawn differently.
    pub perms: Vec<PermRow>,
    /// Where a remembered rule is written — `<harness>/settings.local.json`,
    /// the file `[r]` and `[t]` at an approval prompt write. `None` when the
    /// harness root could not be discovered.
    pub perms_file: Option<String>,
    /// Whether the screen actually read the rules. `false` is the default and
    /// says `not read` rather than printing a confident `none`.
    pub perms_read: bool,
    /// The live context meter: tokens the last call reported, and the cap
    /// compaction is measured against. `None` until a model call has reported
    /// one, which the page prints as `not set` — the status line's own rule.
    pub context: Option<(i64, i64)>,
    /// The running goal's token budget, from the same status the meter comes
    /// from. `None` before a goal has been billed for anything.
    pub goal_budget: Option<i64>,
    /// What the Test Connection button says: untested, reached, or not.
    pub test: TestState,
    /// The focused row, as (card, slot) — slot counts the card's Kv rows.
    /// `None` is the mock's own state.
    pub focus: Option<(usize, usize)>,
    /// A one-line notice under the head rule until dismissed: receipts from
    /// the real controls, honesty from the static ones.
    pub notice: Option<String>,
    /// The Reset two-step: the first Enter arms this, the second fires.
    pub confirm_reset: bool,
    /// One entry per language in `emma_tools_lsp::lang`, read from
    /// settings.json and the filesystem when the screen opens. Empty means the
    /// screen has not read it, which the card says rather than guessing.
    pub lsp: Vec<LspRow>,
    /// Keys in `lsp.enabled` that name no language this build knows. Kept
    /// rather than dropped: a settings file written by a newer Emma is a fact
    /// worth showing, not an error.
    pub lsp_unknown: Vec<String>,
    /// The `training_capture` setting, absent-means-on already resolved by
    /// the caller (`Settings::capture_training`).
    pub training_on: bool,
    /// The `ui.hints` setting, absent-means-on already resolved by the caller
    /// (`Settings::hints`). Whether the interface prints its informational
    /// one-liners; receipts, warnings and refusals are not governed by it.
    pub hints_on: bool,
    /// Every tool this build can register, and what the project's
    /// settings.local.json says about it. Empty means the screen has not read
    /// it, which the card says rather than claiming everything asks.
    pub tools: Vec<(String, ToolState)>,
    /// Where a tool rule would be written, for the receipt. `None` when no
    /// harness root was found here.
    pub tools_file: Option<String>,
    /// The three `memory_policy` settings, defaults already resolved by the
    /// caller.
    pub memory_retention: u64,
    pub auto_recall: bool,
    pub memory_scope: String,
    /// The saved provider's `sampling` entry, **raw**: not resolved, because
    /// absence is what the three rows have to show. `temperature: None` is
    /// "the host decides" and `Some(0.0)` is a chosen zero, and a resolved
    /// value could not tell the row which of those it was holding.
    ///
    /// Keyed by [`sampling_provider`], the provider the next run will boot
    /// with, because that is the run these knobs configure.
    pub sampling_temperature: Option<f64>,
    pub sampling_max_output_tokens: Option<u32>,
    pub sampling_stream: Option<bool>,
    /// The three `appearance` name settings, defaults already resolved by the
    /// caller.
    pub accent: String,
    pub glyphs: String,
    pub status_bar: String,
    /// Whether this terminal is below the xterm cube, so a stored `cube:N`
    /// accent cannot be drawn here. Set at paint from the live palette's
    /// level, which is the only place that fact is known.
    pub no_cube: bool,
    /// The stored font family list the Font Family row cycles, and the one it
    /// is on. Seeded from [`termfont::seed_families_here`] when settings.json
    /// holds none.
    pub font_families: Vec<String>,
    pub font_family: String,
    /// The stored font size, in points.
    pub font_size: u32,
    /// What terminal this is and what it will answer to. Read once, when the
    /// screen opens, because the environment does not change under a running
    /// process and a row must not read it on every draw.
    ///
    /// `Option` rather than a bare `Terminal` because [`termfont::Terminal`]
    /// has no `Default` and inventing one here would mean this page deciding,
    /// for a view nobody has filled in, that the terminal offers no font
    /// control — which is a claim, not an absence. `None` says the screen has
    /// not looked, and the rows say that rather than guessing.
    pub terminal: Option<termfont::Terminal>,
    /// The active keybinding preset and every preset the file offers.
    pub key_preset: String,
    pub key_presets: Vec<String>,
    /// What loading the keybindings file had to say: a refused duplicate, an
    /// action name this build does not know. Empty on a clean load.
    pub key_notes: Vec<String>,
    /// Where the keybindings file is, for the receipts. `None` when no home
    /// directory was found.
    pub keys_file: Option<String>,
}

/// One permission rule as card 6 prints it: the rule text exactly as it is
/// written in the file, and which of the three lists it came from.
///
/// The verdict is `&'static str` from `PermissionKind::word` rather than the
/// enum itself, so this module carries no dependency on the harness crate's
/// vocabulary and a test can name the string it expects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermRow {
    /// `Bash(git *)`, `WebFetch(domain:apnews.com)`, `WebSearch`.
    pub rule: String,
    /// `deny`, `ask` or `allow`.
    pub verdict: &'static str,
}

/// One language on the LANGUAGE SERVERS card: whether it is enabled, and what
/// a filesystem-only look found for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LspRow {
    /// The language's label, as `lang::Language::label` spells it.
    pub label: String,
    /// The key in `lsp.enabled`.
    pub key: String,
    /// In the enabled set, either explicitly or by the default.
    pub enabled: bool,
    /// What was found on disk. Never a claim that the server starts.
    pub found: LspFound,
    /// The language table's `network` flag: this server may reach the network
    /// while answering. It decides which honest notice the row carries.
    pub network: bool,
}

/// What the no-spawn look found. This is `server::Presence` flattened to what
/// the row prints, so the term layer carries no paths it does not draw.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LspFound {
    /// An entry point is on disk and its launcher, if it needs one, is on PATH.
    Found,
    /// An entry point is on disk and the named launcher is not.
    Needs(String),
    /// Nothing on PATH and nothing in an editor extension.
    Absent,
}

/// What one language row's value column says.
///
/// Both dimensions, always, because they are independent and each answers a
/// different question: `Off, found` is a server you have and Emma will not
/// start, and `On, absent` is the opposite and the one that would otherwise
/// read as working. `found` means a file is on disk; see [`NOTICE_LSP_FOUND`].
pub fn lsp_value(row: &LspRow) -> String {
    let on = if row.enabled { "On" } else { "Off" };
    match &row.found {
        LspFound::Found => format!("{on}, found"),
        LspFound::Needs(launcher) => format!("{on}, needs {launcher}"),
        LspFound::Absent => format!("{on}, absent"),
    }
}

/// Whether a provider's key is on this machine, as the Keys Stored row says
/// it. Presence only: the key itself never reaches this layer, so there is
/// nothing here that could be printed by accident.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyPresence {
    /// A key for this provider is in `~/.emma/credentials.json`, or its
    /// environment variable is exported in this shell.
    Stored,
    Missing,
    /// The provider takes no key — a local Ollama, or the `claude` CLI, which
    /// carries its own credentials.
    NotNeeded,
}

impl KeyPresence {
    /// The word the row prints. Never the key, and never a prefix of it.
    pub fn word(self) -> &'static str {
        match self {
            Self::Stored => "yes",
            Self::Missing => "no",
            Self::NotNeeded => "n/a",
        }
    }
}

/// What the permission rules say about one tool, as a row shows it.
///
/// Three states and no fourth, because the file has three lists. `Ask` is the
/// **absence** of a bare-name rule rather than a `permissions.ask` entry: an
/// ask rule forces a prompt even where a session grant already exists, and
/// writing one for every tool somebody never touched would be this screen
/// quietly redefining the default. See [`crate::permissions::set_bare_rule`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ToolState {
    #[default]
    Ask,
    Allow,
    Deny,
}

impl ToolState {
    /// The word the row shows, and the word a test asserts.
    pub fn word(self) -> &'static str {
        match self {
            Self::Ask => "Ask",
            Self::Allow => "Allow",
            Self::Deny => "Deny",
        }
    }
}

/// The three states in the order arrows walk them: absence, then the two
/// rules, weakest first. One place, so the row and the test cannot disagree.
pub const TOOL_STATES: [ToolState; 3] = [ToolState::Ask, ToolState::Allow, ToolState::Deny];

/// The next state from `current`, `dir` steps around [`TOOL_STATES`].
pub fn tool_step(current: ToolState, dir: isize) -> ToolState {
    let n = TOOL_STATES.len() as isize;
    let i = TOOL_STATES.iter().position(|t| *t == current).unwrap_or(0) as isize;
    TOOL_STATES[((i + dir).rem_euclid(n)) as usize]
}

/// The Test Connection button's face. `Idle` says `Test` rather than the
/// mock's `OK`, because an OK nobody measured is a claim about a connection
/// nobody checked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TestState {
    #[default]
    Idle,
    Ok,
    Fail,
}

/// The subtitle under the title, verbatim from the mock.
pub const SUBTITLE: &str = "Configure Emma to match your workflow";

/// The key that leaves this screen, drawn on it. `Esc` is what
/// [`handle_key`] answers with [`SettingsAction::Close`]; `Alt+,` toggles from
/// outside and is not this page's to promise.
pub const EXIT_HINT: &str = "Esc closes";

/// How many cards the grid has, and what Tab wraps within. The ninth is
/// LANGUAGE SERVERS, so the two-by-four grid is a two-by-five with the last
/// right-hand cell empty; the grid paints what there is rather than padding
/// with a card that says nothing.
pub const CARDS: usize = 10;

// endregion: State

// region: The honest notices
// ---------------------------------------------------------------------------
// The honest notices
//
// One constant per claim, so a test asserts the exact sentence and the
// sentence names the mechanism that really exists. Never "not implemented"
// alone.
// ---------------------------------------------------------------------------

/// Where a provider key lives, and — deliberately — no claim about its mode.
///
/// The fork's sentence said *"at mode 600"*. There is no mode 600 on Windows,
/// and a permission bit named on a screen that runs on both is the kind of
/// detail a reader checks and finds absent. What is true everywhere is that
/// the key is never shown here and that one command puts it there.
pub const NOTICE_PROVIDER_KEYS: &str = "Keys live in ~/.emma/credentials.json and are never \
     shown on this screen; `emma set-provider <name> --key -` reads one from stdin, and n/a \
     means the provider needs none";
pub const NOTICE_MODEL: &str =
    "/model lists this provider's models; /model <id> switches this session, --save remembers it";
/// The cap row's mechanism. Named separately from [`NOTICE_CONTEXT_ABSENT`]
/// because the two rows on that card are now different *states*, and one
/// sentence covering both is how the old page ended up calling a live number
/// and a compiled-in sample the same thing.
pub const NOTICE_CONTEXT: &str = "The context cap is this run's --max-context; a request past it \
     compacts the oldest goals down to half the cap, which is why there is no separate threshold";
pub const NOTICE_CONTEXT_ABSENT: &str = "No number yet: the meter reads the provider's own count \
     from the last call, so it is set by the first model call of the run and not before";
pub const NOTICE_OUTPUT_CAP: &str = "Emma has no output-token setting; --max-tokens is the \
     per-goal spend budget shown below it, not a cap on one response";
/// Why the Accent row's chevrons walk five names and no more.
///
/// A `cube:N` accent is a real stored value — `palette::parse_accent` reads
/// one and the palette draws it — and this screen has no picker for it yet.
/// Saying so is the difference between a ladder that is short and a ladder
/// that is lying about the space.
pub const NOTICE_ACCENT: &str = "The chevrons walk the five role accents. A `cube:N` value \
     (16-231) in appearance.accent is also honoured on a 256-colour terminal; there is no \
     picker for one on this screen yet, so it is set by hand";
/// Why a stored cube accent is not on screen right now.
pub const NOTICE_ACCENT_NO_CUBE: &str = "This terminal reports fewer than 256 colours, so a \
     cube accent cannot be drawn here and the theme's own accent is showing instead; the \
     stored value is untouched";
pub const NOTICE_KEYS: &str = "Keybindings live in ~/.emma/keybindings.json, read once at \
     startup: an edit there applies to the next run. Switching between presets in that one \
     read takes effect now. Alt+q is not rebindable, because the way out of a running goal \
     is not a preference";
/// The MEMORY PREFERENCES description. **The forward-setting sentence**, and
/// a constant because it is the only thing keeping three real writes from
/// reading as three live controls: it names what reads each value today.
pub const DESC_MEMORY: &str = "Enable Memory and Capture Training Data are in force now. \
     Retention, Auto-Recall and Scope are stored now and read by nothing yet; nothing prunes \
     today, and the receipt on each says so.";
/// The TOOL PERMISSIONS description. The harness gate's own wording: rules are
/// read at boot, so a change binds the next run.
pub const DESC_PERMISSIONS: &str = "Ask, Allow or Deny per tool, written to settings.local.json. \
     Rules are read at boot, so a change binds the next run; a run already going keeps the gate \
     it booted with. Rules with a specifier, like Bash(cargo *), are left alone.";
/// The MODEL PROVIDER description. **Which provider the three sampling rows
/// edit, and when the edit takes effect**, because neither is guessable from
/// the rows: sampling is keyed per provider and resolved once, in `main.rs`,
/// when the provider is built.
pub const DESC_SAMPLING: &str = "Temperature, Max Output Tokens and Streaming edit the sampling \
     entry for the saved provider, the one Provider names for the next run. Sampling resolves \
     once when the provider is built, so a change binds the next run.";
/// The APPEARANCE description.
///
/// Five mechanisms on one card and each named, because they really are five:
/// two repaint, two wait for a restart, and two leave the process entirely to
/// ask another application for something.
pub const DESC_APPEARANCE: &str = "Theme and Accent repaint now. Glyphs is read once at \
     startup, so it applies to the next run. Font Family and Font Size store the value always \
     and ask the terminal to change its own font where it has a way to be asked. Status Bar \
     and Interface Hints are stored and read by nothing yet; each receipt says so.";
/// The KEYBINDINGS description. Two mechanisms, one row each, and the split
/// between them is the whole content: the file is read once and the preset is
/// not.
pub const DESC_KEYS: &str = "Chords for the Alt layer, from ~/.emma/keybindings.json. The file \
     is read once at startup; the preset switches now. Open writes a commented starter on \
     first use.";
pub const NOTICE_PRUNE: &str = "Prune History drops superseded tool results from what is sent \
     back to the model; off unless set, because it changes what the model is shown";
pub const NOTICE_PERMISSIONS: &str = "Permission rules live in settings.local.json. [r] or [t] \
     at an approval prompt writes one, and it is honoured by every later run in this project";
/// What the tool rows write, and what they will not touch.
///
/// **This replaces `NOTICE_PERMISSIONS_READONLY`, which this package made
/// false.** That sentence — *"This card reads the rules and never writes
/// one"* — was true while the card had no keys, and the argument under it was
/// that a settings route into an execution decision is a second door into a
/// decision that already has a good one. The owner's ruling reverses it for
/// the *bare-name* case only, and the reason it is not the same door: an
/// ask/allow/deny preference per tool is a standing consent, made with the
/// whole tool surface in front of you and nothing running. The prompt is
/// still the only place a *specifier* grant is made, and
/// [`crate::permissions::set_bare_rule`] never touches one.
pub const NOTICE_PERMISSIONS_WRITES: &str = "These rows write a bare-name rule to \
     settings.local.json, the file the approval prompt's [r] and [t] write. A hand-written \
     rule with a specifier, like Bash(cargo *), is never touched by any state of this row";
/// The gate is consent, not containment — `CLAUDE.md` calls documentation
/// implying otherwise a defect, and a screen headed TOOL PERMISSIONS is the
/// most likely place to imply it.
pub const NOTICE_PERMISSIONS_GATE: &str = "These rules decide what Emma asks about, not what a \
     tool can do: the gate is a consent interface and never a sandbox";
pub const NOTICE_PERMISSIONS_NONE: &str = "This project has no permission rules, so every tool \
     call that declares a risk is asked about; nothing here is denied in advance";
pub const NOTICE_PERMISSIONS_UNREAD: &str =
    "Open the screen from inside a project: the rules are read once, from the harness root Emma \
     discovers at that moment";
pub const NOTICE_CWD: &str =
    "The working directory is where Emma was started; start Emma elsewhere to change it";
pub const NOTICE_ENV: &str = "Emma has no environment or log-level setting: there is no \
     dev/staging/prod concept here and no log level to choose";
pub const NOTICE_TELEMETRY: &str = "Emma sends no telemetry; there is nothing to switch off";
pub const NOTICE_LSP: &str = "Language servers are chosen by the lsp.enabled list in \
     settings.json; Emma reads it once at startup, so an edit applies to the next run";
pub const NOTICE_LSP_NETWORK: &str = "terraform, bicep and ansible are off by default because \
     those servers may reach the network while the tools declare they do not; adding the key to \
     lsp.enabled in settings.json is the opt-in";
pub const NOTICE_LSP_FOUND: &str = "found means an entry point is on disk, not that it runs: \
     this screen starts no process, and only a real tool call proves a server answers";
pub const NOTICE_LSP_UNKNOWN: &str = "these lsp.enabled keys name no language this build knows; \
     they are kept, not rejected, and the languages it does know still start";
pub const NOTICE_RESET_CONFIRM: &str = "Reset clears the keys this screen owns in \
     settings.json: theme, accent, glyphs, status bar, fonts, hints, memory capture, training \
     capture, prune history, the memory policy block and the per-provider sampling block. It \
     does NOT touch the provider, the models, or the tool rules in settings.local.json. Enter \
     again confirms, Esc cancels";
pub const NOTICE_RESET_CANCELLED: &str = "Reset cancelled — nothing changed";

/// The Font rows' own honesty, shown when either is focused with no terminal
/// to drive.
///
/// A function rather than a constant because the sentence names the terminal
/// it is looking at, and the fork's constant named Terminal.app
/// unconditionally — false on Windows, where `termfont` has had an arm since
/// F1. The rows are live where they can be; this is what decides which.
pub fn notice_font_scope(terminal: Option<&termfont::Terminal>) -> String {
    let Some(terminal) = terminal else {
        return "The screen has not read this terminal yet, so it cannot say whether a font \
                can be asked for here"
            .to_string();
    };
    if termfont::offers_control(terminal) {
        return format!(
            "Emma cannot draw a font; it asks {} to change its own. A name the terminal does \
             not have is accepted and ignored there, so the receipt says what was asked, not \
             that the window changed",
            terminal.name
        );
    }
    termfont::no_control_note(terminal)
}

// endregion: The honest notices

// region: Card contents
// ---------------------------------------------------------------------------
// Card contents
//
// Data first, painting second, so a test can name a row without parsing a
// buffer. One entry per mock row, in the mock's order.
// ---------------------------------------------------------------------------

/// How a value is dressed. The label side never varies.
#[derive(Debug, Clone)]
enum Value {
    /// Plain accent value: `Model  llama3:8b`.
    Plain(String),
    /// Chevron cycler: `‹ Ollama ›`.
    Cycler(String),
    /// Bracketed button: `[ Save Now ]`.
    Button(String),
    /// A value that does not exist: `n/a`, `not set`, `none`. **Drawn dim
    /// rather than in the accent**, which is the whole of state 3 — a reader
    /// who presses nothing can still see that this row is not a fact about the
    /// run.
    ///
    /// The mock's `Ask ›` chevron dress used to live beside these; it is gone
    /// with the six fabricated verdicts it dressed. A trailing chevron is an
    /// edit affordance, and there was never a key behind it.
    Absent(String),
}

/// What activating a row really does — the class table, one entry per row.
/// The real controls carry their own variant; every static row carries the
/// notice that names its true mechanism.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowKind {
    /// ←/→/Enter step through [`SettingsView::themes`], selected and
    /// persisted.
    ThemeCycle,
    /// Enter/←/→ flip `memory` in settings.json.
    MemoryToggle,
    /// Enter/←/→ flip `prune_history` in settings.json.
    PruneToggle,
    /// Enter/←/→ flip `training_capture`, the `MemoryToggle` pattern: a real
    /// key, absent means on, and the row says what it did.
    TrainingToggle,
    /// Enter/←/→ flip `ui.hints`, on the same rule.
    HintsToggle,
    /// ←/→ step the provider through `emma_llm::kind::known()`, written to
    /// settings.json for the next run. The live binding does not move.
    ProviderCycle,
    /// ←/→ step one tool through [`TOOL_STATES`], written to
    /// settings.local.json. Binds the next run.
    ToolPermission(&'static str),
    /// ←/→ step `memory_policy.retention_days` through [`RETENTION_STEPS`].
    MemoryRetention,
    /// Enter/←/→ flip `memory_policy.auto_recall`.
    AutoRecall,
    /// ←/→ step `memory_policy.scope` through [`MEMORY_SCOPES`].
    MemoryScope,
    /// ←/→ step `appearance.accent` through `palette::ACCENTS`, applied and
    /// persisted — the Theme row's pattern exactly.
    AccentCycle,
    /// ←/→ step `appearance.glyphs` through [`GLYPH_SETS`]. Next run.
    GlyphsCycle,
    /// ←/→ step `appearance.status_bar` through [`STATUS_BARS`].
    StatusBarCycle,
    /// ←/→ step `appearance.font_family` through the stored list.
    FontFamilyCycle,
    /// ←/→ step `appearance.font_size` by one point, clamped. Enter re-asks
    /// for the size already stored, which is the row's only idempotent verb.
    FontSizeStep,
    /// ←/→ step the active keybinding preset through the file's list.
    KeyPresetCycle,
    /// Open ~/.emma/keybindings.json in the editor, writing a commented
    /// starter first if there is none.
    OpenKeybindings,
    /// Show what loading the keybindings file had to say.
    KeyNotes,
    /// ←/→ step the saved provider's `sampling.temperature` through
    /// [`TEMPERATURE_STEPS`]. The first rung removes the key.
    TemperatureCycle,
    /// ←/→ step the saved provider's `sampling.max_output_tokens` through
    /// [`MAX_OUTPUT_STEPS`]. The first rung removes the key.
    MaxOutputCycle,
    /// ←/→ flip the saved provider's `sampling.stream`. On removes the key,
    /// the `memory` rule: absent means on.
    StreamingCycle,
    /// A reachability check against a local Ollama host.
    TestConnection,
    /// Write settings.json now, with a receipt.
    Save,
    /// Write a timestamped copy beside settings.json.
    Export,
    /// Clear the additive keys this screen owns — after a confirm.
    Reset,
    /// Static today: activation shows this notice.
    Note(&'static str),
}

#[derive(Debug, Clone)]
enum CardRow {
    Kv(String, Value, RowKind),
    Divider,
    Desc(String),
}

struct Card {
    header: &'static str,
    rows: Vec<CardRow>,
    /// At most this many Kv rows are drawn at once, scrolled to keep the
    /// focused one in view. `None` draws them all, which is every card but
    /// one.
    ///
    /// **The tool list is as long as the tool registry, and the grid is
    /// not.** TOOL PERMISSIONS draws one row per registered tool — 27 of them
    /// today — and a card 27 rows tall pushes ENVIRONMENT, SAVE & RESET and
    /// LANGUAGE SERVERS off the bottom of any real terminal. The choice was
    /// between a shorter list that lies about the tool surface and a window
    /// over the true one; the window keeps every tool reachable by the same
    /// arrow keys that reach every other row, and the card's description says
    /// how many there are.
    window: Option<usize>,
}

/// How many Kv rows the TOOL PERMISSIONS card shows at once. Sized to leave
/// that card roughly as tall as the MEMORY PREFERENCES card it shares a grid
/// band with, so the band does not grow at all.
const TOOL_WINDOW: usize = 6;

impl Card {
    /// A card that draws all its rows — every card but TOOL PERMISSIONS.
    fn new(header: &'static str, rows: Vec<CardRow>) -> Self {
        Self {
            header,
            rows,
            window: None,
        }
    }

    /// The first Kv row drawn, given where the focus is.
    ///
    /// Deterministic rather than sticky: the same focus always produces the
    /// same window, so a test can assert what is on screen without replaying
    /// the path that got there.
    fn offset(&self, focused_slot: Option<usize>) -> usize {
        let Some(window) = self.window else {
            return 0;
        };
        let total = self.kv_count();
        let last = total.saturating_sub(window);
        match focused_slot {
            Some(slot) => slot.saturating_sub(window / 2).min(last),
            None => 0,
        }
    }

    fn kv_count(&self) -> usize {
        self.rows
            .iter()
            .filter(|r| matches!(r, CardRow::Kv(..)))
            .count()
    }

    /// How many lines the card wants, border included.
    fn height(&self) -> u16 {
        let shown = match self.window {
            Some(window) => self.rows.len() - self.kv_count().saturating_sub(window),
            None => self.rows.len(),
        };
        shown as u16 + 3
    }
}

fn kv(label: &str, v: Value, k: RowKind) -> CardRow {
    CardRow::Kv(label.to_string(), v, k)
}

/// A registry name with its first letter raised, for the value column:
/// `ollama` → `Ollama`, `dracula` → `Dracula`.
fn title_case(name: &str) -> String {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// The Test Connection button's face for each state.
fn test_label(t: TestState) -> &'static str {
    match t {
        TestState::Idle => "Test",
        TestState::Ok => "OK",
        TestState::Fail => "FAIL",
    }
}

/// The cards, in the mock's order for the first eight: left column odd, right
/// column even, top to bottom.
///
/// Every row is one of the module doc's three states. **The balance moved with
/// the write-back package**: what used to be a screen of six live controls and
/// fourteen `n/a` rows is a screen where every appearance, sampling,
/// memory-policy, keybinding and tool row writes a real key. Live and
/// read-only is now the small set — Model, the context numbers, Keys Stored,
/// the keybindings file's path, Telemetry, Working Directory, the rules list
/// on card 6 and every row of card 9. Absent — `Value::Absent`, dim — is what
/// is left: Response Budget, Environment, Log Level, and the context rows
/// before the first model call has reported one.
///
/// **A row that writes a key nothing reads yet is still a live row, and its
/// receipt says so.** Status Bar, Interface Hints and the three memory-policy
/// rows are in that state today. The alternative was `n/a` on a key this
/// screen can genuinely store and a later build will genuinely read, which
/// loses the choice rather than bounding it; [`DESC_MEMORY`] and
/// [`DESC_APPEARANCE`] name the bound on the card itself.
fn cards(s: &SettingsView) -> Vec<Card> {
    use RowKind::Note;
    use Value::{Absent, Button, Cycler, Plain};
    vec![
        Card::new(
            "1. MODEL PROVIDER",
            vec![
                kv(
                    "Provider",
                    Cycler(provider_value(s)),
                    RowKind::ProviderCycle,
                ),
                kv("Model", Plain(s.model.clone()), Note(NOTICE_MODEL)),
                kv(
                    &sampling_label(s, "Temperature"),
                    Cycler(temperature_label(s.sampling_temperature)),
                    RowKind::TemperatureCycle,
                ),
                kv(
                    &sampling_label(s, "Max Output Tokens"),
                    Cycler(max_output_label(s.sampling_max_output_tokens)),
                    RowKind::MaxOutputCycle,
                ),
                kv(
                    &sampling_label(s, "Streaming"),
                    Cycler(on_off(s.sampling_stream.unwrap_or(true)).into()),
                    RowKind::StreamingCycle,
                ),
                kv(
                    "Keys Stored",
                    Plain(keys_value(s)),
                    Note(NOTICE_PROVIDER_KEYS),
                ),
                CardRow::Divider,
                kv(
                    "Test Connection",
                    Button(test_label(s.test).into()),
                    RowKind::TestConnection,
                ),
                CardRow::Desc(DESC_SAMPLING.into()),
            ],
        ),
        Card::new(
            "2. CONTEXT LIMITS",
            vec![
                kv(
                    "Max Context Tokens",
                    match s.context {
                        Some((_, cap)) => Plain(cap.to_string()),
                        None => Absent("not set".into()),
                    },
                    Note(match s.context {
                        Some(_) => NOTICE_CONTEXT,
                        None => NOTICE_CONTEXT_ABSENT,
                    }),
                ),
                kv(
                    "Context In Use",
                    match s.context {
                        Some((used, _)) => Plain(used.to_string()),
                        None => Absent("not set".into()),
                    },
                    Note(match s.context {
                        Some(_) => NOTICE_CONTEXT,
                        None => NOTICE_CONTEXT_ABSENT,
                    }),
                ),
                // The mock's row, kept by name and emptied: Emma has no
                // per-response output cap, and the number that *is* a budget
                // is the goal's, on the row below.
                kv(
                    "Response Budget",
                    Absent("n/a".into()),
                    Note(NOTICE_OUTPUT_CAP),
                ),
                kv(
                    "Goal Token Budget",
                    match s.goal_budget {
                        Some(cap) => Plain(cap.to_string()),
                        None => Absent("not set".into()),
                    },
                    Note(match s.goal_budget {
                        Some(_) => NOTICE_CONTEXT,
                        None => NOTICE_CONTEXT_ABSENT,
                    }),
                ),
                // Unconditional whenever there is a cap at all — see
                // `Agent::compact_if_needed`, which returns early only when
                // `max_context <= 0`.
                kv(
                    "Auto-Summarize",
                    match s.context {
                        Some((_, cap)) if cap > 0 => Plain("On".into()),
                        Some(_) => Plain("Off, no cap".into()),
                        None => Absent("not set".into()),
                    },
                    Note(NOTICE_CONTEXT),
                ),
                // The mock's `70%` was a setting Emma does not have: the
                // threshold *is* the cap, and the half is the target.
                kv(
                    "Summarize Threshold",
                    Plain("at the cap".into()),
                    Note(NOTICE_CONTEXT),
                ),
                CardRow::Desc("Manage how much context Emma can use.".into()),
            ],
        ),
        Card::new(
            "3. APPEARANCE",
            vec![
                kv("Theme", Cycler(title_case(&s.theme)), RowKind::ThemeCycle),
                kv(
                    "Accent Color",
                    Cycler(accent_label(&s.accent)),
                    RowKind::AccentCycle,
                ),
                kv(
                    "Glyphs",
                    Cycler(title_case(&s.glyphs)),
                    RowKind::GlyphsCycle,
                ),
                kv(
                    "Font Family",
                    Cycler(font_family_label(s)),
                    RowKind::FontFamilyCycle,
                ),
                kv(
                    "Font Size",
                    Cycler(font_size_label(s)),
                    RowKind::FontSizeStep,
                ),
                kv(
                    "Status Bar",
                    Cycler(title_case(&s.status_bar)),
                    RowKind::StatusBarCycle,
                ),
                kv(
                    "Interface Hints",
                    Plain(on_off(s.hints_on).into()),
                    RowKind::HintsToggle,
                ),
                CardRow::Desc(DESC_APPEARANCE.into()),
            ],
        ),
        Card::new("4. KEYBINDINGS", keybinding_rows(s)),
        Card::new(
            "5. MEMORY PREFERENCES",
            vec![
                kv(
                    "Enable Memory",
                    Plain(on_off(s.memory_on).into()),
                    RowKind::MemoryToggle,
                ),
                kv(
                    "Capture Training Data",
                    Plain(on_off(s.training_on).into()),
                    RowKind::TrainingToggle,
                ),
                // New here, and the ruling's "even if they were not before"
                // half: `prune_history` is personal settings with no UI until
                // now.
                kv(
                    "Prune History",
                    Plain(on_off(s.prune_on).into()),
                    RowKind::PruneToggle,
                ),
                kv(
                    "Memory Retention",
                    Cycler(retention_label(s.memory_retention)),
                    RowKind::MemoryRetention,
                ),
                kv(
                    "Auto-Recall",
                    Plain(on_off(s.auto_recall).into()),
                    RowKind::AutoRecall,
                ),
                kv(
                    "Memory Scope",
                    Cycler(title_case(&s.memory_scope)),
                    RowKind::MemoryScope,
                ),
                CardRow::Desc(DESC_MEMORY.into()),
            ],
        ),
        Card {
            header: "6. TOOL PERMISSIONS",
            rows: tool_rows(s),
            window: Some(TOOL_WINDOW),
        },
        Card::new(
            "7. ENVIRONMENT",
            vec![
                kv("Working Directory", Plain(s.cwd.clone()), Note(NOTICE_CWD)),
                kv("Environment", Absent("n/a".into()), Note(NOTICE_ENV)),
                kv("Log Level", Absent("n/a".into()), Note(NOTICE_ENV)),
                // Not a switch that happens to be off: there is no sender.
                kv(
                    "Telemetry",
                    Plain("none sent".into()),
                    Note(NOTICE_TELEMETRY),
                ),
                CardRow::Desc("Environment and runtime configuration.".into()),
            ],
        ),
        Card::new(
            "8. SAVE & RESET",
            vec![
                kv("Save Settings", Button("Save Now".into()), RowKind::Save),
                kv("Export Settings", Button("Export".into()), RowKind::Export),
                CardRow::Divider,
                kv("Reset to Defaults", Button("Reset".into()), RowKind::Reset),
                CardRow::Desc("Reset will restore all settings to defaults.".into()),
            ],
        ),
        Card::new("9. LANGUAGE SERVERS", lsp_rows(s)),
        Card::new("10. RULES IN FORCE", perm_rows(s)),
    ]
}

/// The Provider row's value: **both truths when they differ**.
///
/// One name when the session is running what the file says, and
/// `Ollama (running) -> Openrouter (next run)` when it is not. The row could
/// have shown the saved name alone and read as a switch, or the live name
/// alone and read as a control that does nothing; neither is what happened.
/// Rebinding the live client is `main.rs` machinery, and a settings screen
/// that half-did it would be worse than one that says what it did.
fn provider_value(s: &SettingsView) -> String {
    if s.provider_saved.is_empty() || s.provider_saved == s.provider {
        return title_case(&s.provider);
    }
    format!(
        "{} (running) -> {} (next run)",
        title_case(&s.provider),
        title_case(&s.provider_saved)
    )
}

/// The Accent row's value.
///
/// A role name is title-cased like every other name on the screen; a cube
/// accent shows its index, because there is no name to show. `Cube 81` rather
/// than `81` so the number is never read as a size or a count.
fn accent_label(accent: &str) -> String {
    match accent.strip_prefix(super::palette::ACCENT_CUBE_PREFIX) {
        Some(i) => format!("Cube {i}"),
        None => title_case(accent),
    }
}

/// The Font Family row's value: the family, and `(stored)` where there is no
/// terminal to ask.
///
/// **Both truths where they differ**, the Provider row's rule. A row showing
/// `Menlo` alone under Windows Terminal would read as a setting that took, and
/// one showing nothing would hide a value that really is stored and really
/// will be asked for the next time Emma runs somewhere that can be asked.
fn font_family_label(s: &SettingsView) -> String {
    if offers_font_control(s) {
        return s.font_family.clone();
    }
    format!("{} (stored)", s.font_family)
}

/// The Font Size row's value, on the same rule.
fn font_size_label(s: &SettingsView) -> String {
    if offers_font_control(s) {
        return format!("{} pt", s.font_size);
    }
    format!("{} pt (stored)", s.font_size)
}

/// Whether the terminal this screen read has a font control Emma can drive.
/// A view nobody filled in answers `false` and the rows say `(stored)`, which
/// is the honest reading of "nobody looked".
fn offers_font_control(s: &SettingsView) -> bool {
    s.terminal.as_ref().is_some_and(termfont::offers_control)
}

/// The KEYBINDINGS card's rows.
///
/// The Load Notes row exists only when the load had something to say. A row
/// permanently reading `0 notes` would be noise on every clean start, and the
/// one time it matters is the time it appears.
fn keybinding_rows(s: &SettingsView) -> Vec<CardRow> {
    let mut rows = vec![
        kv(
            "Keybinding Preset",
            Value::Cycler(title_case(&s.key_preset)),
            RowKind::KeyPresetCycle,
        ),
        kv(
            "Open Keybindings",
            Value::Button("Open".into()),
            RowKind::OpenKeybindings,
        ),
        kv(
            "Keybindings File",
            match &s.keys_file {
                Some(path) => Value::Plain(path.clone()),
                None => Value::Absent("no home directory".into()),
            },
            RowKind::Note(NOTICE_KEYS),
        ),
    ];
    if !s.key_notes.is_empty() {
        let n = s.key_notes.len();
        rows.push(kv(
            "Load Notes",
            Value::Plain(format!("{n} refused")),
            RowKind::KeyNotes,
        ));
    }
    rows.push(CardRow::Desc(DESC_KEYS.into()));
    rows
}

/// The Keys Stored row's value: every provider this build can run, named,
/// with a yes/no beside it.
///
/// Every provider rather than only the configured ones: "openrouter no" is
/// the answer somebody opening this screen is looking for, and a list of the
/// yes-set alone leaves them guessing what the set was.
fn keys_value(s: &SettingsView) -> String {
    if s.provider_keys.is_empty() {
        return "not read".to_string();
    }
    s.provider_keys
        .iter()
        .map(|(name, presence)| format!("{name} {}", presence.word()))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The two words a toggle row shows. One function so the toggles cannot spell
/// it several ways.
fn on_off(v: bool) -> &'static str {
    if v {
        "On"
    } else {
        "Off"
    }
}

/// How many rules card 6 draws before it stops and says how many are left.
///
/// A cap rather than a scroll because the grid does not scroll: a card that
/// grew to a hundred rows would push every card below it off the screen, and
/// the overflow notice would then be counting whole cards rather than rules.
/// The house rule for a cap is the same wherever it appears — name it, name
/// the loss, name the remedy — which is what [`NOTICE_PERMISSIONS_MORE`] does.
pub const PERM_ROWS_SHOWN: usize = 8;

pub const NOTICE_PERMISSIONS_MORE: &str = "This card shows the first rules only; no key raises \
     the cap. Open the file named above to read them all";

/// The TOOL PERMISSIONS card's rows: one Ask/Allow/Deny cycler per tool this
/// build can register, and the file they are written to.
///
/// **This card writes now, and the argument for the old read-only rule is
/// half kept.** It used to say that editing a permission from here would be a
/// settings route into an execution decision — a second door into a decision
/// that already has one good door, the approval prompt, which has the call
/// that wants the grant on screen beside it. What the owner's ruling
/// separates is *which* decision. A bare-name Ask/Allow/Deny is a standing
/// preference about a whole tool, chosen with the tool surface in front of
/// you and nothing running; a **specifier** grant — `Bash(cargo *)`,
/// `WebFetch(domain:docs.rs)` — answers a question about one call, and that
/// one is still made only at the prompt. [`crate::permissions::set_bare_rule`]
/// enforces the split: it removes and writes bare rules only, and never
/// touches a hand-written specifier in any list.
///
/// **The tool list is derived, never hand-written.** One row per
/// [`crate::runctl::ALL_TOOLS`] entry, which is the same list `Deny All`
/// writes and the registry test holds: a tool added to Emma appears here with
/// nobody remembering to add it. The card's `window` is what makes 27 rows
/// fit a grid cell.
fn tool_rows(s: &SettingsView) -> Vec<CardRow> {
    use RowKind::Note;
    use Value::{Absent, Cycler, Plain};
    let mut rows: Vec<CardRow> = crate::runctl::ALL_TOOLS
        .iter()
        .map(|name| {
            kv(
                name,
                Cycler(tool_state(s, name).word().to_string()),
                RowKind::ToolPermission(name),
            )
        })
        .collect();
    rows.push(CardRow::Divider);
    rows.push(kv(
        "Written To",
        match &s.tools_file {
            Some(path) => Plain(path.clone()),
            None => Absent("no harness root".into()),
        },
        Note(NOTICE_PERMISSIONS_WRITES),
    ));
    rows.push(CardRow::Desc(DESC_PERMISSIONS.into()));
    rows
}

/// The RULES IN FORCE card's rows: every rule the gate will really consult,
/// in the order it consults them, read-only.
///
/// **A separate card from the cyclers, and the split is the disclosure.** The
/// cyclers say what this project's `settings.local.json` holds for each tool;
/// this says what is in force, which is that file *merged with the spine
/// file beside the harness* — a document this screen must not write, and one
/// whose `deny` outranks a local `allow`. Folding the two into one card meant
/// one of them was below a window and therefore invisible until somebody
/// scrolled, and the read-only half is the half a reader has to be able to
/// see without pressing anything.
fn perm_rows(s: &SettingsView) -> Vec<CardRow> {
    use RowKind::Note;
    use Value::{Absent, Plain};
    let mut rows: Vec<CardRow> = Vec::new();
    if !s.perms_read {
        rows.push(kv(
            "Rules",
            Absent("not read".into()),
            Note(NOTICE_PERMISSIONS_UNREAD),
        ));
        rows.push(CardRow::Desc(
            "The rules are read from the harness root when the screen opens.".into(),
        ));
        return rows;
    }
    if s.perms.is_empty() {
        rows.push(kv(
            "Rules",
            Absent("none".into()),
            Note(NOTICE_PERMISSIONS_NONE),
        ));
    } else {
        for row in s.perms.iter().take(PERM_ROWS_SHOWN) {
            rows.push(kv(
                &row.rule,
                Plain(row.verdict.to_string()),
                Note(NOTICE_PERMISSIONS_GATE),
            ));
        }
        if s.perms.len() > PERM_ROWS_SHOWN {
            rows.push(kv(
                "Not Shown",
                Absent(format!("{} more", s.perms.len() - PERM_ROWS_SHOWN)),
                Note(NOTICE_PERMISSIONS_MORE),
            ));
        }
    }
    rows.push(CardRow::Divider);
    rows.push(kv(
        "Rules File",
        match &s.perms_file {
            Some(path) => Plain(path.clone()),
            None => Absent("no harness root".into()),
        },
        Note(NOTICE_PERMISSIONS),
    ));
    rows.push(kv(
        "Specifier Rules",
        Plain("only at the prompt".into()),
        Note(NOTICE_PERMISSIONS_WRITES),
    ));
    rows.push(CardRow::Desc(
        "Read-only: card 6 writes bare names, and a specifier is granted at the prompt.".into(),
    ));
    rows
}

/// What the rules say about one tool, as the view holds it. A tool the screen
/// has no entry for reads as [`ToolState::Ask`], which is the absence of a
/// bare rule and is exactly what an unread file means.
fn tool_state(s: &SettingsView, tool: &str) -> ToolState {
    s.tools
        .iter()
        .find(|(t, _)| t == tool)
        .map(|(_, st)| *st)
        .unwrap_or_default()
}

/// The LANGUAGE SERVERS card's rows: one per language, the unknown keys when
/// there are any, and a summary that counts what the rows above say.
///
/// Every row is [`RowKind::Note`] by design. See the module doc's class table:
/// nothing here writes, because the pool that would have to act on a write is
/// built once in `main.rs` from the file as it was at startup.
fn lsp_rows(s: &SettingsView) -> Vec<CardRow> {
    use RowKind::Note;
    use Value::Plain;
    if s.lsp.is_empty() {
        return vec![
            kv("Languages", Plain("not read".into()), Note(NOTICE_LSP)),
            CardRow::Desc("Open the screen to read lsp.enabled from settings.json.".into()),
        ];
    }
    let mut rows: Vec<CardRow> = s
        .lsp
        .iter()
        .map(|row| {
            kv(
                &row.label,
                Plain(lsp_value(row)),
                Note(if row.network {
                    NOTICE_LSP_NETWORK
                } else {
                    NOTICE_LSP
                }),
            )
        })
        .collect();
    if !s.lsp_unknown.is_empty() {
        rows.push(kv(
            "Unknown Keys",
            Plain(s.lsp_unknown.join(", ")),
            Note(NOTICE_LSP_UNKNOWN),
        ));
    }
    let enabled = s.lsp.iter().filter(|r| r.enabled).count();
    let found = s
        .lsp
        .iter()
        .filter(|r| r.enabled && r.found == LspFound::Found)
        .count();
    rows.push(CardRow::Divider);
    rows.push(kv(
        "Found On Disk",
        Plain(format!("{found} of {enabled} on")),
        Note(NOTICE_LSP_FOUND),
    ));
    rows.push(CardRow::Desc(
        "lsp.enabled in settings.json chooses; found means on disk, not proven.".into(),
    ));
    rows
}

// ---------------------------------------------------------------------------
// The ladders
//
// One walker, `name_step`, and one table per row that steps names — so a
// provider, an accent, a glyph set and a scope cannot each grow their own
// off-by-one. The numeric ladders are fixed rungs rather than plus-or-minus
// one, because the useful values are far apart and thirty presses is not a
// control; each lands a hand-written value on the nearest rung at or above it,
// so a number typed into settings.json is stepped *from* rather than snapped
// to the start.
// ---------------------------------------------------------------------------

/// The next name from `current`, `dir` steps around `names`. A name the ladder
/// does not hold steps from index 0, which is where an unknown name resolves
/// anyway.
fn name_step(names: &[&'static str], current: &str, dir: isize) -> &'static str {
    if names.is_empty() {
        return "";
    }
    let n = names.len() as isize;
    let i = names.iter().position(|t| *t == current).unwrap_or(0) as isize;
    names[((i + dir).rem_euclid(n)) as usize]
}

/// Every provider this build can run, in the order the row cycles them.
///
/// Read from the registry rather than listed here: a provider added to
/// `emma_llm::kind::KINDS` becomes reachable from this row with no second list
/// to forget, which is the `ALL_TOOLS` lesson applied before it can be
/// repeated.
pub fn providers() -> Vec<&'static str> {
    emma_llm::kind::known()
}

/// The next provider from `current`.
pub fn provider_step(current: &str, dir: isize) -> &'static str {
    name_step(&providers(), current, dir)
}

/// What `Memory Retention` steps through, in days. `0` is Keep Forever, the
/// stored default; see [`crate::settings::RETENTION_KEEP_FOREVER`].
pub const RETENTION_STEPS: [u64; 5] = [0, 7, 30, 90, 365];

/// The next retention from `current`, `dir` steps around [`RETENTION_STEPS`].
pub fn retention_step(current: u64, dir: isize) -> u64 {
    let at = RETENTION_STEPS
        .iter()
        .position(|d| *d >= current)
        .unwrap_or(RETENTION_STEPS.len() - 1);
    let n = RETENTION_STEPS.len() as isize;
    RETENTION_STEPS[(at as isize + dir).rem_euclid(n) as usize]
}

/// How a retention reads on the row.
fn retention_label(days: u64) -> String {
    if days == crate::settings::RETENTION_KEEP_FOREVER {
        "Keep Forever".to_string()
    } else {
        format!("{days} days")
    }
}

/// What `Memory Scope` steps through, and the first entry is the default.
pub const MEMORY_SCOPES: [&str; 2] = ["project", "global"];

/// What `Glyphs` steps through. `auto` is the detection Emma already does;
/// see `settings::AppearanceSettings::glyphs` for why `unicode` is a request
/// rather than a guarantee.
pub const GLYPH_SETS: [&str; 3] = ["auto", "unicode", "ascii"];

/// What `Status Bar` steps through. Two, and the missing third is the point:
/// a hidden status bar would take the mode cell and `q quit` off the screen,
/// which are the two things that hold the row at every width.
pub const STATUS_BARS: [&str; 2] = ["full", "compact"];

/// The provider whose `sampling` entry the three rows edit.
///
/// The **saved** provider, not the running one, because these knobs are read
/// once when a provider is built and the next build is the saved provider's.
/// Editing the running provider's entry would write knobs for a run that is
/// already past the only moment it could read them.
pub fn sampling_provider(s: &SettingsView) -> &str {
    if s.provider_saved.is_empty() {
        &s.provider
    } else {
        &s.provider_saved
    }
}

/// A sampling row's label, naming the provider when the saved and the running
/// halves disagree.
///
/// Silent while they agree, for the reason the Provider row shows one name
/// then: a suffix on every row would be noise, and the case worth spending
/// width on is the one where "which provider is this" has two answers.
fn sampling_label(s: &SettingsView, base: &str) -> String {
    if s.provider_saved.is_empty() || s.provider_saved == s.provider {
        base.to_string()
    } else {
        format!("{base} ({})", s.provider_saved)
    }
}

/// What `Temperature` steps through, in hundredths. `None` is the first rung:
/// the key absent, so nothing reaches the wire and the host decides.
///
/// **Hundredths rather than `f64` so the ladder is exact and the action that
/// carries a rung can be compared.** [`SettingsAction`] is `Eq`, which a float
/// cannot be, and a ladder of literals a test has to compare with an epsilon
/// is a ladder that can drift a rung without saying so.
pub const TEMPERATURE_STEPS: [Option<u32>; 6] =
    [None, Some(0), Some(20), Some(40), Some(70), Some(100)];

/// The next temperature rung from `current`, `dir` steps around
/// [`TEMPERATURE_STEPS`].
pub fn temperature_step(current: Option<f64>, dir: isize) -> Option<u32> {
    let at = match current {
        None => 0,
        Some(t) => {
            let hundredths = (t * 100.0).round().max(0.0) as u32;
            TEMPERATURE_STEPS
                .iter()
                .position(|r| matches!(r, Some(v) if *v >= hundredths))
                .unwrap_or(TEMPERATURE_STEPS.len() - 1)
        }
    };
    let n = TEMPERATURE_STEPS.len() as isize;
    TEMPERATURE_STEPS[(at as isize + dir).rem_euclid(n) as usize]
}

/// A rung as the file holds it: hundredths back to the number that goes out.
pub fn temperature_value(rung: u32) -> f64 {
    f64::from(rung) / 100.0
}

/// How a temperature reads on the row: the provenance, honestly.
///
/// "Host default" is not a number this build knows. Anthropic, Ollama,
/// OpenRouter and OpenAI each apply their own and the four do not agree, so
/// printing one would be Emma inventing a fact about somebody else's server.
fn temperature_label(t: Option<f64>) -> String {
    match t {
        None => "Host default".to_string(),
        Some(t) => format!("{t:.2}"),
    }
}

/// What `Max Output Tokens` steps through. `None` is the first rung: the key
/// absent, which means [`crate::settings::EMMA_MAX_OUTPUT_TOKENS`].
pub const MAX_OUTPUT_STEPS: [Option<u32>; 5] =
    [None, Some(4096), Some(8192), Some(16384), Some(65536)];

/// The next output cap from `current`, [`temperature_step`]'s rule.
pub fn max_output_step(current: Option<u32>, dir: isize) -> Option<u32> {
    let at = match current {
        None => 0,
        Some(n) => MAX_OUTPUT_STEPS
            .iter()
            .position(|r| matches!(r, Some(v) if *v >= n))
            .unwrap_or(MAX_OUTPUT_STEPS.len() - 1),
    };
    let n = MAX_OUTPUT_STEPS.len() as isize;
    MAX_OUTPUT_STEPS[(at as isize + dir).rem_euclid(n) as usize]
}

/// How an output cap reads on the row.
///
/// The default rung names its number, unlike Temperature's: this value is
/// always sent, on every provider, so the row can say what it is. That is the
/// whole difference between a host default and an Emma default, shown rather
/// than explained.
fn max_output_label(n: Option<u32>) -> String {
    match n {
        None => format!("Default ({})", crate::settings::EMMA_MAX_OUTPUT_TOKENS),
        Some(n) => n.to_string(),
    }
}

/// The next accent name from `current`, `dir` steps around `palette::ACCENTS`.
///
/// The chevrons stay the five-role ladder. A `cube:N` accent is not on that
/// ladder, so a chevron press from one steps back onto it at the first rung —
/// the same rule an unknown theme name has, and [`NOTICE_ACCENT`] says so on
/// the row.
fn accent_step(current: &str, dir: isize) -> &'static str {
    let names: Vec<&'static str> = super::palette::ACCENTS.iter().map(|a| a.name).collect();
    name_step(&names, current, dir)
}

/// The next font family from the stored list, `dir` steps around it.
///
/// Borrowed from the view rather than returning a `&'static str`, because the
/// list is somebody's typing and there is no static to point at.
fn font_family_step(s: &SettingsView, dir: isize) -> &str {
    if s.font_families.is_empty() {
        return &s.font_family;
    }
    let list = &s.font_families;
    let n = list.len() as isize;
    let i = list.iter().position(|f| *f == s.font_family).unwrap_or(0) as isize;
    &list[((i + dir).rem_euclid(n)) as usize]
}

/// The next keybinding preset, `dir` steps around the file's list.
fn key_preset_step(s: &SettingsView, dir: isize) -> &str {
    if s.key_presets.is_empty() {
        return &s.key_preset;
    }
    let n = s.key_presets.len() as isize;
    let i = s
        .key_presets
        .iter()
        .position(|p| *p == s.key_preset)
        .unwrap_or(0) as isize;
    &s.key_presets[((i + dir).rem_euclid(n)) as usize]
}

/// How many focusable (Kv) rows card `c` has.
fn slots(s: &SettingsView, c: usize) -> usize {
    cards(s)
        .get(c)
        .map(|card| {
            card.rows
                .iter()
                .filter(|r| matches!(r, CardRow::Kv(..)))
                .count()
        })
        .unwrap_or(0)
}

/// Where the row carrying `kind` sits on card `card`, as [`SettingsView::focus`]
/// counts slots.
///
/// Exported for the shell's tests: a slot number written out by hand is the
/// thing that goes stale when a row is added above it, and this package added
/// eleven. `None` when the card has no such row, so a caller says which one it
/// could not find rather than focusing something else.
pub fn row_slot(s: &SettingsView, card: usize, kind: RowKind) -> Option<usize> {
    cards(s).get(card).and_then(|c| {
        c.rows
            .iter()
            .filter_map(|r| match r {
                CardRow::Kv(_, _, k) => Some(*k),
                _ => None,
            })
            .position(|k| k == kind)
    })
}

/// The focused row's class, if a row is focused.
fn focused_kind(s: &SettingsView) -> Option<RowKind> {
    let (c, slot) = s.focus?;
    cards(s).get(c).and_then(|card| {
        card.rows
            .iter()
            .filter_map(|r| match r {
                CardRow::Kv(_, _, k) => Some(*k),
                _ => None,
            })
            .nth(slot)
    })
}

// endregion: Card contents

// region: Keys
// ---------------------------------------------------------------------------
// Keys — the pure half of the seam, `memory::handle_key`'s shape.
// ---------------------------------------------------------------------------

/// What a key asks the shell to do. Everything that needs disk or network
/// crosses this enum; [`handle_key`] never touches a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettingsAction {
    /// Not this page's key (or a release/chord): let it fall through.
    None,
    /// The view changed (focus, notice, confirm) and wants a repaint.
    FocusChanged,
    /// Esc with nothing focused: close the screen.
    Close,
    /// Select and persist this theme — exactly what `/theme <name>` does,
    /// including its "from the next start" effect. `String` rather than
    /// `&'static str`: a theme name is a filename here, not a table entry.
    Theme(String),
    /// Store `memory`: `true` removes the key (absent means on), `false`
    /// writes `false`.
    MemoryCapture(bool),
    /// Store `prune_history`: `false` removes the key (absent means off),
    /// `true` writes `true`. The mirror image of [`Self::MemoryCapture`],
    /// because the two defaults are opposite.
    PruneHistory(bool),
    /// Store `training_capture`, on [`Self::MemoryCapture`]'s rule: `true`
    /// removes the key (absent means on), `false` writes `false`.
    TrainingCapture(bool),
    /// Store `ui.hints`, on the same rule.
    Hints(bool),
    /// Store `provider` for the next run. The live client is not rebound.
    Provider(&'static str),
    /// Store one tool's bare-name rule in settings.local.json.
    ToolPolicy(&'static str, ToolState),
    /// Store `memory_policy.retention_days`.
    Retention(u64),
    /// Store `memory_policy.auto_recall`.
    AutoRecall(bool),
    /// Store `memory_policy.scope`.
    MemoryScope(&'static str),
    /// Apply and persist an accent override: a role name from
    /// `palette::ACCENTS`.
    ///
    /// A `String` rather than a `&'static str` because the same path has to
    /// carry a `cube:N` value a hand-edited settings file may hold, and there
    /// is no static to point at for a number somebody chose.
    Accent(String),
    /// Store `appearance.glyphs`, for the next run.
    Glyphs(&'static str),
    /// Store `appearance.status_bar`.
    StatusBar(&'static str),
    /// Ask the terminal for a font family, and store it.
    FontFamily(String),
    /// Ask the terminal for a font size, and store it.
    FontSize(u32),
    /// Switch the active keybinding preset, now, and remember the name for
    /// this screen's row.
    KeyPreset(String),
    /// Open ~/.emma/keybindings.json in the editor.
    OpenKeybindings,
    /// Store the saved provider's `sampling.temperature`, in hundredths.
    /// `None` removes the key: the host decides, and 0 is not that.
    Temperature(Option<u32>),
    /// Store the saved provider's `sampling.max_output_tokens`. `None`
    /// removes the key, which means the Emma default.
    MaxOutputTokens(Option<u32>),
    /// Store the saved provider's `sampling.stream`. `true` removes the key
    /// (absent means on), `false` writes `false`.
    Streaming(bool),
    /// Ping the provider's host and report into the button.
    TestConnection,
    /// Write settings.json now, receipt in the notice.
    Save,
    /// Write a timestamped copy beside settings.json.
    Export,
    /// The confirmed Reset: clear the additive keys this screen owns.
    Reset,
}

/// One key, against the open Settings screen. Alt/Ctrl chords and releases
/// are never this page's: they return [`SettingsAction::None`] untouched so
/// the global layer (Alt+s, Ctrl-C…) keeps working over an open page.
pub fn handle_key(v: &mut SettingsView, key: KeyEvent) -> SettingsAction {
    if key.kind == KeyEventKind::Release
        || key
            .modifiers
            .intersects(KeyModifiers::ALT | KeyModifiers::CONTROL)
    {
        return SettingsAction::None;
    }
    match key.code {
        KeyCode::Tab => {
            v.confirm_reset = false;
            let c = v.focus.map_or(0, |(c, _)| (c + 1) % CARDS);
            v.focus = Some((c, 0));
            SettingsAction::FocusChanged
        }
        KeyCode::BackTab => {
            v.confirm_reset = false;
            let c = v.focus.map_or(CARDS - 1, |(c, _)| (c + CARDS - 1) % CARDS);
            v.focus = Some((c, 0));
            SettingsAction::FocusChanged
        }
        KeyCode::Down => step(v, 1),
        KeyCode::Up => step(v, -1),
        KeyCode::Left => cycle(v, -1),
        KeyCode::Right => cycle(v, 1),
        KeyCode::Enter => activate(v),
        KeyCode::Esc => {
            if v.confirm_reset {
                v.confirm_reset = false;
                v.notice = Some(NOTICE_RESET_CANCELLED.to_string());
                SettingsAction::FocusChanged
            } else if v.focus.is_some() || v.notice.is_some() {
                v.focus = None;
                v.notice = None;
                SettingsAction::FocusChanged
            } else {
                SettingsAction::Close
            }
        }
        _ => SettingsAction::None,
    }
}

/// ↑/↓ within the focused card, clamped at its edges — memory's convention.
/// With nothing focused, ↓ enters the first card.
fn step(v: &mut SettingsView, dir: isize) -> SettingsAction {
    v.confirm_reset = false;
    let Some((c, slot)) = v.focus else {
        if dir > 0 {
            v.focus = Some((0, 0));
        }
        return SettingsAction::FocusChanged;
    };
    let n = slots(v, c);
    let next = slot.saturating_add_signed(dir).min(n.saturating_sub(1));
    v.focus = Some((c, next));
    SettingsAction::FocusChanged
}

/// ←/→ on the focused row: the theme really cycles, the toggle flips, and
/// every static row answers with its honest notice.
fn cycle(v: &mut SettingsView, dir: isize) -> SettingsAction {
    v.confirm_reset = false;
    match focused_kind(v) {
        Some(RowKind::ThemeCycle) => SettingsAction::Theme(theme_step(&v.themes, &v.theme, dir)),
        Some(RowKind::MemoryToggle) => SettingsAction::MemoryCapture(!v.memory_on),
        Some(RowKind::PruneToggle) => SettingsAction::PruneHistory(!v.prune_on),
        Some(RowKind::TrainingToggle) => SettingsAction::TrainingCapture(!v.training_on),
        Some(RowKind::HintsToggle) => SettingsAction::Hints(!v.hints_on),
        // Stepped from the *saved* name, not the running one: the ladder has
        // to walk from where the last press left it, or a second press would
        // step back onto the booted provider.
        Some(RowKind::ProviderCycle) => {
            SettingsAction::Provider(provider_step(sampling_provider(v), dir))
        }
        Some(RowKind::ToolPermission(tool)) => {
            SettingsAction::ToolPolicy(tool, tool_step(tool_state(v, tool), dir))
        }
        Some(RowKind::MemoryRetention) => {
            SettingsAction::Retention(retention_step(v.memory_retention, dir))
        }
        Some(RowKind::AutoRecall) => SettingsAction::AutoRecall(!v.auto_recall),
        Some(RowKind::MemoryScope) => {
            SettingsAction::MemoryScope(name_step(&MEMORY_SCOPES, &v.memory_scope, dir))
        }
        Some(RowKind::AccentCycle) => {
            SettingsAction::Accent(accent_step(&v.accent, dir).to_string())
        }
        Some(RowKind::GlyphsCycle) => {
            SettingsAction::Glyphs(name_step(&GLYPH_SETS, &v.glyphs, dir))
        }
        Some(RowKind::StatusBarCycle) => {
            SettingsAction::StatusBar(name_step(&STATUS_BARS, &v.status_bar, dir))
        }
        Some(RowKind::FontFamilyCycle) => {
            SettingsAction::FontFamily(font_family_step(v, dir).to_string())
        }
        Some(RowKind::FontSizeStep) => {
            SettingsAction::FontSize(termfont::step_size(v.font_size, dir))
        }
        Some(RowKind::KeyPresetCycle) => {
            SettingsAction::KeyPreset(key_preset_step(v, dir).to_string())
        }
        Some(RowKind::TemperatureCycle) => {
            SettingsAction::Temperature(temperature_step(v.sampling_temperature, dir))
        }
        Some(RowKind::MaxOutputCycle) => {
            SettingsAction::MaxOutputTokens(max_output_step(v.sampling_max_output_tokens, dir))
        }
        Some(RowKind::StreamingCycle) => {
            SettingsAction::Streaming(!v.sampling_stream.unwrap_or(true))
        }
        Some(RowKind::Note(text)) => {
            v.notice = Some(text.to_string());
            SettingsAction::FocusChanged
        }
        // ←/→ on a button does nothing; Enter is its verb.
        Some(_) | None => SettingsAction::None,
    }
}

/// Enter on the focused row.
fn activate(v: &mut SettingsView) -> SettingsAction {
    match focused_kind(v) {
        // Enter steps forward on every cycler and flips every toggle, the
        // Theme row's convention: one verb, so a row cannot mean one thing to
        // the arrows and another to Enter.
        Some(
            RowKind::ThemeCycle
            | RowKind::MemoryToggle
            | RowKind::PruneToggle
            | RowKind::TrainingToggle
            | RowKind::HintsToggle
            | RowKind::ProviderCycle
            | RowKind::ToolPermission(_)
            | RowKind::MemoryRetention
            | RowKind::AutoRecall
            | RowKind::MemoryScope
            | RowKind::AccentCycle
            | RowKind::GlyphsCycle
            | RowKind::StatusBarCycle
            | RowKind::FontFamilyCycle
            | RowKind::KeyPresetCycle
            | RowKind::TemperatureCycle
            | RowKind::MaxOutputCycle
            | RowKind::StreamingCycle,
        ) => {
            v.confirm_reset = false;
            cycle(v, 1)
        }
        // The one idempotent Enter on the screen. This row's verb is the
        // arrows, and Enter re-asks the terminal for the size already stored,
        // which is what somebody presses it for after switching windows.
        Some(RowKind::FontSizeStep) => {
            v.confirm_reset = false;
            SettingsAction::FontSize(v.font_size)
        }
        Some(RowKind::OpenKeybindings) => {
            v.confirm_reset = false;
            SettingsAction::OpenKeybindings
        }
        Some(RowKind::KeyNotes) => {
            v.confirm_reset = false;
            v.notice = Some(v.key_notes.join(" | "));
            SettingsAction::FocusChanged
        }
        Some(RowKind::TestConnection) => {
            v.confirm_reset = false;
            SettingsAction::TestConnection
        }
        Some(RowKind::Save) => {
            v.confirm_reset = false;
            SettingsAction::Save
        }
        Some(RowKind::Export) => {
            v.confirm_reset = false;
            SettingsAction::Export
        }
        Some(RowKind::Reset) => {
            if v.confirm_reset {
                v.confirm_reset = false;
                SettingsAction::Reset
            } else {
                v.confirm_reset = true;
                v.notice = Some(NOTICE_RESET_CONFIRM.to_string());
                SettingsAction::FocusChanged
            }
        }
        Some(RowKind::Note(text)) => {
            v.confirm_reset = false;
            v.notice = Some(text.to_string());
            SettingsAction::FocusChanged
        }
        None => SettingsAction::None,
    }
}

/// The next theme name from `current`, `dir` steps around `themes`. An unknown
/// current name steps from the first entry, which is the built-in — and is
/// where an unknown name resolves anyway.
///
/// An empty list answers with the built-in rather than panicking on a modulus
/// by zero. `theme::names` cannot return one; a caller that built the view by
/// hand can.
fn theme_step(themes: &[String], current: &str, dir: isize) -> String {
    if themes.is_empty() {
        return "emma".to_string();
    }
    let n = themes.len() as isize;
    let i = themes.iter().position(|t| t == current).unwrap_or(0) as isize;
    themes[((i + dir).rem_euclid(n)) as usize].clone()
}

// endregion: Keys

// region: Mouse
// ---------------------------------------------------------------------------
// Mouse — rects recorded at paint, a pure hit-test after (the harness's
// render_hits pattern: paint and hit-test cannot drift).
// ---------------------------------------------------------------------------

/// Where the controls were on the last paint. Recorded by [`render_hits`]
/// from the same arithmetic that painted them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Hits {
    /// Every control rect, most specific first: a row's chevrons and value
    /// before the row itself, so [`hit`]'s first match is the right one.
    pub controls: Vec<(Rect, Hit)>,
}

/// What a left-button press means, given where the controls were painted.
/// The (card, slot) pair is [`SettingsView::focus`]'s own coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hit {
    /// A row's label side: focus it.
    Row(usize, usize),
    /// A cycler's left chevron: focus and step back (the ← key).
    Prev(usize, usize),
    /// A cycler's right chevron: focus and step forward (the → key).
    Next(usize, usize),
    /// A button, toggle or value: focus and activate (the Enter key).
    Act(usize, usize),
}

/// The pure hit-test. `None` for every other cell — a page where one control
/// works and the rest swallows clicks is worse than one the pointer passes
/// through (the sidebar's rule).
pub fn hit(hits: &Hits, col: u16, row: u16) -> Option<Hit> {
    let inside = |r: &Rect| col >= r.x && col < r.x + r.width && row >= r.y && row < r.y + r.height;
    hits.controls
        .iter()
        .find(|(r, _)| inside(r))
        .map(|(_, h)| *h)
}

// endregion: Mouse

// region: Rendering
// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

/// Draw the whole screen into `area`, dropping the control rects — the
/// convenience wrapper the pure-painting tests use.
pub fn render(area: Rect, buf: &mut Buffer, s: &SettingsView, skin: &Skin) {
    let _ = render_hits(area, buf, s, skin);
}

/// Draw the whole screen and report where every control landed. The shell
/// stores the result and hit-tests presses against it. Too small an area
/// draws what fits from the top, clipped whole-row like the sidebar.
pub fn render_hits(area: Rect, buf: &mut Buffer, s: &SettingsView, skin: &Skin) -> Hits {
    if area.width < 4 || area.height < 2 {
        return Hits::default();
    }
    let w = usize::from(area.width);
    // The head: version in the corner, the title, the subtitle, a rule, and
    // the notice row the interactions speak through.
    let head_h = 5.min(area.height);
    let [head, grid] =
        Layout::vertical([Constraint::Length(head_h), Constraint::Min(0)]).areas(area);
    let mut head_lines: Vec<Line<'static>> = vec![corner_row(EXIT_HINT, &s.version, w, skin)];
    // "Very large" is not a thing a terminal cell can do; one bold accent row
    // is this repository's standing substitute (design Q8: no figlet).
    head_lines.push(Line::from(Span::styled(
        fit("Settings", w, skin.glyphs.ellipsis),
        skin.palette.bold(Role::Accent),
    )));
    head_lines.push(Line::from(Span::styled(
        fit(SUBTITLE, w, skin.glyphs.ellipsis),
        skin.palette.dim(),
    )));
    head_lines.push(Line::from(Span::styled(
        skin.glyphs.rule.repeat(w),
        skin.palette.dim(),
    )));
    // The notice row: receipts and honesty, in the accent so they read as an
    // answer (the harness's ruling); absent, the row stays empty air.
    if let Some(n) = &s.notice {
        head_lines.push(Line::from(Span::styled(
            fit(n, w, skin.glyphs.ellipsis),
            skin.palette.style(Role::Accent),
        )));
    }
    for (i, line) in head_lines.iter().take(usize::from(head.height)).enumerate() {
        buf.set_line(head.x, head.y + i as u16, line, head.width);
    }
    if grid.height == 0 {
        return Hits::default();
    }
    render_grid(grid, buf, s, skin)
}

/// The out-of-room notice, one row, at the bottom of the grid.
///
/// **A page that ran out of room says how much is missing and how to get it.**
/// This grid clips whole card-rows from the bottom and, until 2026-08-27, said
/// nothing at all: at 80x24 — an ordinary default — the last cards were simply
/// absent, unreachable by any key, with no sentence saying they existed. The
/// page does not scroll, so a taller window is the only remedy there is, and a
/// count is what lets a reader judge whether resizing is worth it. Item A1 of
/// the term-hardening backport checklist; the guarantee predates the TUI
/// import, did not survive it, and is restored here rather than papered over in
/// the test that found it missing.
fn overflow_line(hidden: usize, w: usize, skin: &Skin) -> Line<'static> {
    Line::from(Span::styled(
        fit(
            &format!(
                "{} {hidden} more {} below — this page does not scroll; make the window taller",
                skin.glyphs.ellipsis,
                if hidden == 1 { "card" } else { "cards" },
            ),
            w,
            skin.glyphs.ellipsis,
        ),
        skin.palette.dim(),
    ))
}

/// The two-by-four grid. Each grid row is as tall as the taller of its two
/// cards; a pane too short clips from the bottom, whole rows at a time, and
/// [`overflow_line`] says how many cards went with them.
fn render_grid(area: Rect, buf: &mut Buffer, s: &SettingsView, skin: &Skin) -> Hits {
    let cards = cards(s);
    // Two passes over the same arithmetic, because the notice has to be
    // *reserved* before the cards are placed or it paints over the last one.
    // The first pass asks how many pairs fit in the whole grid; if that is all
    // of them there is nothing to say and no row is taken.
    let placed = |height: u16| {
        let mut y = 0u16;
        let mut pairs = 0usize;
        for pair in cards.chunks(2) {
            if y >= height {
                break;
            }
            let tallest = pair.iter().map(Card::height).max().unwrap_or(0);
            let h = tallest.min(height - y);
            if h < 3 {
                break;
            }
            y += h;
            pairs += 1;
        }
        pairs
    };
    let total_pairs = cards.chunks(2).len();
    let notice = u16::from(placed(area.height) < total_pairs);
    let grid_h = area.height - notice;
    let mut hits = Hits::default();
    let [left_col, _, right_col] = Layout::horizontal([
        Constraint::Fill(1),
        Constraint::Length(1),
        Constraint::Fill(1),
    ])
    .areas(area);
    let mut y = area.y;
    let mut drawn = 0usize;
    for (pair_i, pair) in cards.chunks(2).enumerate() {
        if y >= area.y + grid_h {
            break;
        }
        let tallest = pair.iter().map(Card::height).max().unwrap_or(0);
        let room = (area.y + grid_h).saturating_sub(y);
        let h = tallest.min(room);
        if h < 3 {
            break;
        }
        for (i, card) in pair.iter().enumerate() {
            let col = if i == 0 { left_col } else { right_col };
            let index = pair_i * 2 + i;
            render_card(
                Rect::new(col.x, y, col.width, h),
                buf,
                card,
                index,
                s,
                skin,
                &mut hits,
            );
        }
        y += h;
        drawn += pair.len();
    }
    if notice > 0 && drawn < cards.len() {
        let line = overflow_line(cards.len() - drawn, usize::from(area.width), skin);
        buf.set_line(area.x, area.y + area.height - 1, &line, area.width);
    }
    hits
}

/// One painted line and, for a Kv row, its slot plus the value's column
/// offset and width — what the hit rects are recorded from.
type RowLine = (Line<'static>, Option<(usize, usize, usize)>);

/// One bordered card: the numbered header in the accent, then the rows. The
/// card holding the focus wears an accent border and its focused row the
/// sidebar's chip band; every Kv row reports its rects into `hits` from the
/// same arithmetic that painted it.
fn render_card(
    area: Rect,
    buf: &mut Buffer,
    card: &Card,
    index: usize,
    s: &SettingsView,
    skin: &Skin,
    hits: &mut Hits,
) {
    if area.width < 4 || area.height < 3 {
        return;
    }
    let focused_card = matches!(s.focus, Some((c, _)) if c == index);
    let block = Block::bordered()
        .border_set(skin.glyphs.border)
        .border_style(if focused_card {
            skin.palette.style(Role::Accent)
        } else {
            skin.palette.dim()
        });
    let inner = block.inner(area);
    block.render(area, buf);
    let w = usize::from(inner.width);
    // Each line, with its Kv slot and value geometry when it has one.
    let mut lines: Vec<RowLine> = Vec::new();
    lines.push((
        Line::from(Span::styled(
            fit(card.header, w, skin.glyphs.ellipsis),
            skin.palette.bold(Role::Accent),
        )),
        None,
    ));
    let focused_slot = match s.focus {
        Some((c, slot)) if c == index => Some(slot),
        _ => None,
    };
    let offset = card.offset(focused_slot);
    let window = card.window.unwrap_or(usize::MAX);
    let mut slot = 0usize;
    for row in &card.rows {
        // Outside the window: the row keeps its slot number, so focus and the
        // hit rects stay in the card's own coordinates whatever is on screen.
        if matches!(row, CardRow::Kv(..))
            && (slot < offset || slot >= offset.saturating_add(window))
        {
            slot += 1;
            continue;
        }
        lines.push(match row {
            CardRow::Kv(label, value, _) => {
                let hot = s.focus == Some((index, slot));
                let (line, vx, vw) = kv_line(label, value, w, skin, hot);
                let entry = (line, Some((slot, vx, vw)));
                slot += 1;
                entry
            }
            CardRow::Divider => (
                Line::from(Span::styled(skin.glyphs.rule.repeat(w), skin.palette.dim())),
                None,
            ),
            CardRow::Desc(text) => (
                Line::from(Span::styled(
                    fit(text, w, skin.glyphs.ellipsis),
                    skin.palette.dim(),
                )),
                None,
            ),
        });
    }
    // The mock puts the description at the *bottom* of the card. A card that
    // was padded to its grid-row partner's height keeps it there: the body is
    // drawn from the top and a trailing description sinks to the last inner
    // row, never overlapping the body when the card is squeezed.
    let sunk_desc = matches!(card.rows.last(), Some(CardRow::Desc(_)));
    let body = if sunk_desc {
        lines.len() - 1
    } else {
        lines.len()
    };
    let mut dresses = card
        .rows
        .iter()
        .filter_map(|r| match r {
            CardRow::Kv(_, v, _) => Some(v),
            _ => None,
        })
        .skip(offset);
    for (i, (line, kv)) in lines
        .iter()
        .take(body.min(usize::from(inner.height)))
        .enumerate()
    {
        let y = inner.y + i as u16;
        buf.set_line(inner.x, y, line, inner.width);
        if let Some((slot, vx, vw)) = *kv {
            let value = dresses.next().expect("one dress per Kv slot");
            record_hits(hits, inner, y, index, slot, value, vx, vw);
        }
    }
    if sunk_desc {
        let row = (usize::from(inner.height).saturating_sub(1)).max(body);
        if row < usize::from(inner.height) {
            buf.set_line(inner.x, inner.y + row as u16, &lines[body].0, inner.width);
        }
    }
}

/// One Kv row's control rects, most specific first: a cycler's chevrons, a
/// button/toggle/value, then the whole row as the focus target.
#[allow(clippy::too_many_arguments)]
fn record_hits(
    hits: &mut Hits,
    inner: Rect,
    y: u16,
    card: usize,
    slot: usize,
    value: &Value,
    vx: usize,
    vw: usize,
) {
    let vx = inner.x + vx as u16;
    let vw16 = vw as u16;
    match value {
        // The chevrons are the cycler's verbs; the text between them only
        // focuses, so a click cannot change a theme by aiming at its name.
        Value::Cycler(_) if vw >= 4 => {
            hits.controls
                .push((Rect::new(vx, y, 2, 1), Hit::Prev(card, slot)));
            hits.controls
                .push((Rect::new(vx + vw16 - 2, y, 2, 1), Hit::Next(card, slot)));
        }
        // Buttons and toggles activate; a static row's value answers with
        // its notice — the same thing Enter does, one dispatch path.
        Value::Button(_) | Value::Plain(_) | Value::Absent(_) => {
            hits.controls
                .push((Rect::new(vx, y, vw16, 1), Hit::Act(card, slot)));
        }
        Value::Cycler(_) => {}
    }
    hits.controls
        .push((Rect::new(inner.x, y, inner.width, 1), Hit::Row(card, slot)));
}

/// `Label … value` at exactly `w` columns, value right-aligned in the accent.
/// The value survives and the label truncates — the sidebar's trailing-column
/// ruling, for the same reason: values line up as a column and the eye reads
/// them as one. Returns the value's column offset and width so the hit rects
/// record the paint's own arithmetic.
fn kv_line(
    label: &str,
    value: &Value,
    w: usize,
    skin: &Skin,
    hot: bool,
) -> (Line<'static>, usize, usize) {
    let ascii = skin.glyphs == ASCII;
    let (open, close) = if ascii {
        ("< ", " >")
    } else {
        ("‹ ", " ›")
    };
    let (text, mut style) = match value {
        Value::Plain(v) => (v.clone(), skin.palette.style(Role::Accent)),
        Value::Cycler(v) => (
            format!("{open}{v}{close}"),
            skin.palette.style(Role::Accent),
        ),
        Value::Button(v) => (format!("[ {v} ]"), skin.palette.bold(Role::Accent)),
        // **The one place state 3 is visible without pressing anything.** The
        // accent is what the eye reads as "this is a fact about the run"; an
        // absent value must not borrow it.
        Value::Absent(v) => (v.clone(), skin.palette.dim()),
    };
    // The focused row wears the sidebar's chip band — memory's accent
    // treatment for the row the keys are pointed at.
    let mut label_style = skin.palette.style(Role::Text);
    if hot {
        let band = skin.palette.chip(Role::Accent);
        style = band;
        label_style = band;
    }
    // The value is capped at the full row less one column of air; the label
    // takes what is left.
    let value_text = clip(&text, w.saturating_sub(1).max(1), skin);
    let v_w = cols(&value_text);
    let label_text = clip(label, w.saturating_sub(v_w + 1), skin);
    let gap = w.saturating_sub(cols(&label_text) + v_w);
    let line = Line::from(vec![
        Span::styled(label_text, label_style),
        Span::styled(" ".repeat(gap), if hot { style } else { Style::default() }),
        Span::styled(value_text, style),
    ]);
    (line, w - v_w, v_w)
}

/// [`fit`], made total for budgets narrower than the ellipsis — the sidebar's
/// `clipped`, duplicated because that one is private to its module.
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

    fn skin() -> Skin {
        Skin::new(Palette::new(Level::Truecolor), UNICODE)
    }

    fn view() -> SettingsView {
        SettingsView {
            version: "v0.6.3".into(),
            model: "llama3:8b".into(),
            cwd: "~/projects/research".into(),
            provider: "ollama".into(),
            theme: "dracula".into(),
            memory_on: true,
            prune_on: false,
            test: TestState::Ok,
            lsp: lsp_fixture(),
            context: Some((41_000, 120_000)),
            goal_budget: Some(200_000),
            perms: perm_fixture(),
            perms_file: Some("/repo/.emma/settings.local.json".into()),
            perms_read: true,
            provider_saved: "ollama".into(),
            provider_keys: vec![
                ("anthropic".into(), KeyPresence::Missing),
                ("ollama".into(), KeyPresence::NotNeeded),
            ],
            training_on: true,
            hints_on: true,
            tools: vec![("Bash".into(), ToolState::Deny)],
            tools_file: Some("/repo/.emma/settings.local.json".into()),
            memory_retention: 0,
            auto_recall: true,
            memory_scope: "project".into(),
            accent: "theme".into(),
            glyphs: "auto".into(),
            status_bar: "full".into(),
            font_families: vec!["Menlo".into(), "Cascadia Mono".into()],
            font_family: "Menlo".into(),
            font_size: 13,
            terminal: Some(termfont::Terminal {
                name: "Terminal.app".into(),
                control: termfont::Control::AppleScript,
            }),
            key_preset: "default".into(),
            key_presets: vec!["default".into(), "vim".into()],
            keys_file: Some("/home/.emma/keybindings.json".into()),
            ..SettingsView::default()
        }
    }

    /// Every Kv row of `card`, with the slot number the keys use.
    ///
    /// Derived from [`cards`] rather than written out, because the slot
    /// numbers moved when the write-back package landed and a table of
    /// literals is the thing that goes quietly stale when they move again.
    fn rows_of(v: &SettingsView, card: usize) -> Vec<(usize, String, RowKind)> {
        cards(v)
            .into_iter()
            .nth(card)
            .expect("a card index this screen has")
            .rows
            .into_iter()
            .filter_map(|r| match r {
                CardRow::Kv(label, _, kind) => Some((label, kind)),
                _ => None,
            })
            .enumerate()
            .map(|(slot, (label, kind))| (slot, label, kind))
            .collect()
    }

    /// The slot the row with this kind sits at.
    fn slot_of(v: &SettingsView, card: usize, want: RowKind) -> usize {
        rows_of(v, card)
            .into_iter()
            .find(|(_, _, k)| *k == want)
            .unwrap_or_else(|| panic!("card {card} has no {want:?} row"))
            .0
    }

    /// Two rules, one of each of the two verdicts a reader most needs to tell
    /// apart. Stated rather than read from disk for the reason the language
    /// fixture is: a test that reads the developer's own project is a test
    /// that answers differently on every machine.
    fn perm_fixture() -> Vec<PermRow> {
        vec![
            PermRow {
                rule: "Bash(rm *)".into(),
                verdict: "deny",
            },
            PermRow {
                rule: "WebSearch".into(),
                verdict: "allow",
            },
        ]
    }

    /// The seven languages as the screen sees them, one of each state, so a
    /// test never depends on what is installed on the machine running it.
    fn lsp_fixture() -> Vec<LspRow> {
        let row = |label: &str, enabled: bool, found: LspFound, network: bool| LspRow {
            label: label.into(),
            key: label.to_ascii_lowercase(),
            enabled,
            found,
            network,
        };
        vec![
            row("Rust", true, LspFound::Absent, false),
            row("Bash", true, LspFound::Found, false),
            row("PowerShell", true, LspFound::Needs("pwsh".into()), false),
            row("Python", true, LspFound::Absent, false),
            row("Terraform", false, LspFound::Found, true),
            row("Bicep", false, LspFound::Needs("dotnet".into()), true),
            row("Ansible", false, LspFound::Found, true),
        ]
    }

    fn draw(s: &SettingsView, w: u16, h: u16) -> Vec<String> {
        let area = Rect::new(0, 0, w, h);
        let mut buf = Buffer::empty(area);
        render(area, &mut buf, s, &skin());
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

    const HEADERS: [&str; 9] = [
        "1. MODEL PROVIDER",
        "2. CONTEXT LIMITS",
        "3. APPEARANCE",
        "4. KEYBINDINGS",
        "5. MEMORY PREFERENCES",
        "6. TOOL PERMISSIONS",
        "7. ENVIRONMENT",
        "8. SAVE & RESET",
        "9. LANGUAGE SERVERS",
    ];

    /// The mock's grid, plus the ninth card: all numbered headers, odd cards in the left
    /// column and even cards in the right, reading order preserved.
    #[test]
    fn the_eight_card_headers_render_in_the_mocks_order() {
        let rows = draw(&view(), 120, 90);
        let mid = 120 / 2;
        let mut found: Vec<(usize, usize)> = Vec::new(); // (row, col)
        for h in HEADERS {
            let hit = rows
                .iter()
                .enumerate()
                .find_map(|(y, r)| r.find(h).map(|x| (y, x)))
                .unwrap_or_else(|| panic!("{h} not rendered"));
            found.push(hit);
        }
        for (i, (y, x)) in found.iter().enumerate() {
            if i % 2 == 0 {
                assert!(*x < mid, "{} is not in the left column", HEADERS[i]);
            } else {
                assert!(*x >= mid, "{} is not in the right column", HEADERS[i]);
            }
            if i >= 2 {
                assert!(
                    *y > found[i - 2].0,
                    "{} is not below {}",
                    HEADERS[i],
                    HEADERS[i - 2]
                );
            }
        }
        // Paired cards share a band.
        for i in (0..8).step_by(2) {
            assert_eq!(
                found[i].0,
                found[i + 1].0,
                "cards {i} and {} misaligned",
                i + 1
            );
        }
    }

    /// Values end flush against the card's inner right edge — the mock's
    /// right-aligned value column, asserted against the border glyph beside it.
    #[test]
    fn values_right_align_against_the_card_border() {
        let rows = draw(&view(), 120, 60);
        for (label, value) in [
            ("Max Context Tokens", "120000"),
            ("Log Level", "n/a"),
            ("Telemetry", "none sent"),
        ] {
            let row = rows
                .iter()
                .find(|r| r.contains(label))
                .unwrap_or_else(|| panic!("{label} not rendered"));
            let v = row.find(value).unwrap_or_else(|| panic!("{value} missing"));
            let after = &row[v + value.len()..];
            assert!(
                after.trim_start_matches(['│', '|']).len() < after.len()
                    && after
                        .chars()
                        .next()
                        .map(|c| c == '│' || c == '|')
                        .unwrap_or(false),
                "{value} is not flush against the border: {row:?}"
            );
        }
    }

    /// **An edit affordance is drawn only where a key answers it.** The mock
    /// dressed five rows this way; three of them had nothing behind them, and
    /// a chevron with no handler is the `QUICK HELP` defect this repository
    /// has already had a bug report about.
    ///
    /// **The negative half moved, and had to.** It used to name five dead
    /// dresses — `‹ Ollama ›`, `‹ Default ›`, `[ Open ]` and the invented tool
    /// chevrons — and four of those five are live controls now, so keeping
    /// the list would pin the page to the absence this package exists to
    /// remove. What survives is the rule the list was an instance of,
    /// asserted over every row the screen builds: a chevron or a button
    /// appears exactly where a key crosses the seam, and an `Absent` value
    /// never wears one.
    #[test]
    fn an_edit_affordance_is_drawn_only_where_a_key_answers_it() {
        let v = view();
        let all = draw(&v, 130, 60).join("\n");
        assert!(all.contains("‹ Dracula ›"), "theme cycler missing");
        for b in ["[ OK ]", "[ Save Now ]", "[ Export ]", "[ Reset ]"] {
            assert!(all.contains(b), "button {b} missing");
        }
        let mut dressed = 0usize;
        for card in 0..CARDS {
            for row in cards(&v).into_iter().nth(card).expect("a card").rows {
                let CardRow::Kv(label, value, kind) = row else {
                    continue;
                };
                let wears_affordance = matches!(value, Value::Cycler(_) | Value::Button(_));
                let answered = !matches!(kind, RowKind::Note(_));
                // A chevron or a bracket is a promise, so it may only appear
                // on a row a key really changes. The converse does not hold
                // and never did: a toggle is `Plain` — `On` / `Off` with no
                // chevron — because its two states *are* its affordance, and
                // that is mainline's own dress for Enable Memory.
                assert!(
                    !wears_affordance || answered,
                    "{label:?} on card {card} is dressed as editable and no key changes it"
                );
                assert!(
                    !matches!(value, Value::Absent(_)) || !answered,
                    "{label:?} says it has no value and offers a key that changes it"
                );
                dressed += usize::from(wears_affordance);
            }
        }
        assert!(dressed > 20, "only {dressed} live controls on the page");
    }

    /// **Nothing on the page reads as a value that is not one.** The mock's
    /// nineteen sample figures were compiled into the paint path and painted
    /// in the accent beside the live ones; this is the assertion that they are
    /// gone, by the strings they were.
    ///
    /// It is a whole-page negative rather than a per-row check on purpose: a
    /// per-row check passes the moment somebody adds a twentieth.
    #[test]
    fn the_mocks_sample_figures_are_not_on_the_page() {
        let all = draw(&view(), 161, 95).join("\n");
        for sample in [
            "0.70",          // Temperature
            "2048",          // Max Output Tokens / Response Budget
            "8192",          // Max Context Tokens
            "70%",           // Summarize Threshold
            "Magenta",       // Accent Color
            "JetBrains",     // Font Family
            "14px",          // Font Size
            "Detailed",      // Status Bar
            "Disabled",      // Telemetry
            "File Browser",  // a permission row about a launcher
            "Data Explorer", // ditto
        ] {
            assert!(
                !all.contains(sample),
                "the mock's sample {sample:?} is still painted:\n{all}"
            );
        }
        // `local` and `Info` are the other two, and both are substrings of
        // strings that legitimately appear (`settings.local.json`), so they
        // are checked on their own rows rather than across the page.
        // `30 days`, `Project` and `0.70` left this list with the write-back:
        // each is now a rung a person can really select, and asserting their
        // absence would pin the page to the `n/a` it replaced.
        for label in ["Environment", "Log Level", "Response Budget"] {
            let row = draw(&view(), 161, 95)
                .into_iter()
                .find(|r| r.contains(label))
                .unwrap_or_else(|| panic!("{label} not rendered"));
            assert!(row.contains("n/a"), "{label} still reads: {row:?}");
        }
    }

    /// State 3 is visible without pressing anything: an absent value is dim
    /// and a live one is in the accent.
    ///
    /// **This is the guarantee the whole ruling rests on.** Read from the
    /// rendered cells rather than from [`Value`], because the claim is about
    /// what a reader sees — a test over the enum would pass over a renderer
    /// that painted both the same.
    #[test]
    fn an_absent_value_is_dim_and_a_live_one_is_not() {
        let v = view();
        let area = Rect::new(0, 0, 161, 95);
        let mut buf = Buffer::empty(area);
        render(area, &mut buf, &v, &skin());
        let style_of = |needle: &str| -> Style {
            for y in 0..area.height {
                let row: String = (0..area.width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect();
                if let Some(x) = row.find(needle) {
                    // `find` is a byte offset and the card borders are
                    // multi-byte, so count characters up to it.
                    let col = row[..x].chars().count() as u16;
                    return buf[(col, y)].style();
                }
            }
            panic!("{needle} is not on the page");
        };
        // The cell carries a resolved background the role's `Style` does not,
        // so the two are compared on what the role actually sets: the
        // foreground and the modifiers.
        let seen = |s: Style| (s.fg, s.add_modifier);
        let dim = seen(skin().palette.dim());
        let accent = seen(skin().palette.style(Role::Accent));
        assert_ne!(dim, accent, "the test cannot tell the two roles apart");
        // `n/a` on Temperature, and the model, which is this run's.
        assert_eq!(
            seen(style_of("n/a")),
            dim,
            "an absent value borrows the accent"
        );
        assert_eq!(
            seen(style_of("llama3:8b")),
            accent,
            "a live value is not in the accent"
        );
    }

    /// The head block: version top-right, title, subtitle, rule.
    #[test]
    fn the_head_is_version_title_subtitle_rule() {
        let rows = draw(&view(), 100, 50);
        assert!(
            rows[0].ends_with("v0.6.3"),
            "version not in the corner: {:?}",
            rows[0]
        );
        assert_eq!(rows[1], "Settings");
        assert_eq!(rows[2], SUBTITLE);
        assert!(rows[3].chars().all(|c| c == '─'), "rule missing");
    }

    /// The live rows carry the live values.
    #[test]
    fn the_live_fields_are_the_callers_not_the_mocks() {
        let mut v = view();
        v.model = "qwen3:32b".into();
        v.cwd = "/work/elsewhere".into();
        let all = draw(&v, 130, 60).join("\n");
        assert!(all.contains("qwen3:32b"));
        assert!(all.contains("/work/elsewhere"));
        assert!(!all.contains("llama3:8b"));
    }

    /// Nothing overruns: every rendered row fits the width, at sizes from the
    /// mock's own down to degenerate.
    #[test]
    fn no_row_is_ever_wider_than_the_area() {
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
            for row in draw(&view(), w, h) {
                assert!(
                    cols(&row) <= usize::from(w),
                    "row overruns at {w}x{h}: {row:?}"
                );
            }
        }
    }

    // -- keys: focus ---------------------------------------------------------

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::from(code)
    }

    /// Tab walks the eight cards in the mock's order and wraps; BackTab walks
    /// them backwards. Entering a card lands on its first row.
    #[test]
    fn tab_cycles_the_eight_cards_in_order_and_wraps() {
        let mut v = view();
        for c in 0..CARDS {
            assert_eq!(
                handle_key(&mut v, press(KeyCode::Tab)),
                SettingsAction::FocusChanged
            );
            assert_eq!(v.focus, Some((c, 0)), "Tab {c} landed elsewhere");
        }
        handle_key(&mut v, press(KeyCode::Tab));
        assert_eq!(v.focus, Some((0, 0)), "Tab did not wrap");
        handle_key(&mut v, press(KeyCode::BackTab));
        assert_eq!(v.focus, Some((CARDS - 1, 0)), "BackTab did not wrap back");
    }

    /// ↑/↓ move within the focused card and clamp at its edges: ↓ stops on the
    /// card's last row and never walks into the next card.
    ///
    /// The row count is asked of the page rather than written out. It was six
    /// when this test was written and is eight now, and a literal here would
    /// have gone on passing at the wrong number by clamping to it.
    #[test]
    fn arrows_move_within_a_card_and_clamp() {
        let mut v = view();
        let last = slots(&v, 0) - 1;
        handle_key(&mut v, press(KeyCode::Tab));
        for _ in 0..last + 4 {
            handle_key(&mut v, press(KeyCode::Down));
        }
        assert_eq!(v.focus, Some((0, last)), "↓ escaped card 1 or overclamped");
        for _ in 0..last + 4 {
            handle_key(&mut v, press(KeyCode::Up));
        }
        assert_eq!(v.focus, Some((0, 0)), "↑ escaped card 1");
    }

    /// Esc is two-step: it clears focus (and notice) first, and only a second
    /// Esc with nothing focused asks the shell to close the screen.
    #[test]
    fn esc_clears_focus_then_closes() {
        let mut v = view();
        handle_key(&mut v, press(KeyCode::Tab));
        v.notice = Some("something".into());
        assert_eq!(
            handle_key(&mut v, press(KeyCode::Esc)),
            SettingsAction::FocusChanged
        );
        assert_eq!(v.focus, None, "first Esc must clear focus");
        assert_eq!(v.notice, None, "first Esc must clear the notice");
        assert_eq!(
            handle_key(&mut v, press(KeyCode::Esc)),
            SettingsAction::Close,
            "second Esc must close"
        );
    }

    /// Chords and releases are never this page's — the global layer keeps
    /// Alt+s and friends over an open page.
    #[test]
    fn chords_and_releases_fall_through() {
        let mut v = view();
        let mut alt = press(KeyCode::Tab);
        alt.modifiers = KeyModifiers::ALT;
        assert_eq!(handle_key(&mut v, alt), SettingsAction::None);
        let mut release = press(KeyCode::Enter);
        release.kind = KeyEventKind::Release;
        assert_eq!(handle_key(&mut v, release), SettingsAction::None);
        assert_eq!(v.focus, None);
    }

    // -- keys: the class table ----------------------------------------------

    fn focus(v: &mut SettingsView, card: usize, slot: usize) {
        v.focus = Some((card, slot));
    }

    /// Class A: the Theme cycler asks the shell for the real neighbouring
    /// theme in [`SettingsView::themes`], both directions, wrapping.
    ///
    /// The list is the view's rather than a compiled-in table — a theme is a
    /// file here — so the fixture states one. Three entries, because two would
    /// make the wrap and the step indistinguishable.
    #[test]
    fn the_theme_cycler_steps_through_the_palette_registry() {
        let mut v = view();
        v.themes = vec!["emma".into(), "oxide".into(), "house".into()];
        v.theme = "emma".to_string();
        focus(&mut v, 2, 0);
        assert_eq!(
            handle_key(&mut v, press(KeyCode::Right)),
            SettingsAction::Theme("oxide".to_string())
        );
        assert_eq!(
            handle_key(&mut v, press(KeyCode::Left)),
            SettingsAction::Theme("house".to_string()),
            "← must wrap to the last theme"
        );
        assert_eq!(
            handle_key(&mut v, press(KeyCode::Enter)),
            SettingsAction::Theme("oxide".to_string()),
            "Enter cycles forward too"
        );
    }

    /// Class A: Enable Memory asks for the opposite of what is shown —
    /// the shell owns the absent-means-on encoding.
    #[test]
    fn the_memory_toggle_asks_for_the_flip() {
        let mut v = view();
        focus(&mut v, 4, 0);
        assert_eq!(
            handle_key(&mut v, press(KeyCode::Enter)),
            SettingsAction::MemoryCapture(false)
        );
        v.memory_on = false;
        assert_eq!(
            handle_key(&mut v, press(KeyCode::Right)),
            SettingsAction::MemoryCapture(true)
        );
    }

    /// Class A: the buttons cross the seam as themselves.
    #[test]
    fn the_buttons_ask_for_their_real_actions() {
        let mut v = view();
        let test = slot_of(&v, 0, RowKind::TestConnection);
        focus(&mut v, 0, test);
        assert_eq!(
            handle_key(&mut v, press(KeyCode::Enter)),
            SettingsAction::TestConnection
        );
        focus(&mut v, 7, 0);
        assert_eq!(
            handle_key(&mut v, press(KeyCode::Enter)),
            SettingsAction::Save
        );
        focus(&mut v, 7, 1);
        assert_eq!(
            handle_key(&mut v, press(KeyCode::Enter)),
            SettingsAction::Export
        );
    }

    /// Reset is a two-step: the first Enter arms the confirm and says so, the
    /// second fires, and Esc in between cancels without firing.
    #[test]
    fn reset_confirms_before_firing_and_esc_cancels() {
        let mut v = view();
        focus(&mut v, 7, 2);
        assert_eq!(
            handle_key(&mut v, press(KeyCode::Enter)),
            SettingsAction::FocusChanged
        );
        assert!(v.confirm_reset, "first Enter must arm the confirm");
        assert_eq!(v.notice.as_deref(), Some(NOTICE_RESET_CONFIRM));
        assert_eq!(
            handle_key(&mut v, press(KeyCode::Enter)),
            SettingsAction::Reset,
            "second Enter must fire"
        );
        assert!(!v.confirm_reset);
        // Armed again, Esc cancels rather than closing anything.
        handle_key(&mut v, press(KeyCode::Enter));
        assert!(v.confirm_reset);
        assert_eq!(
            handle_key(&mut v, press(KeyCode::Esc)),
            SettingsAction::FocusChanged
        );
        assert!(!v.confirm_reset, "Esc must disarm");
        assert_eq!(v.notice.as_deref(), Some(NOTICE_RESET_CANCELLED));
        // And moving focus disarms too: a confirm must not survive aiming at
        // a different control.
        handle_key(&mut v, press(KeyCode::Enter));
        assert!(v.confirm_reset);
        handle_key(&mut v, press(KeyCode::Up));
        assert!(!v.confirm_reset, "moving focus must disarm the confirm");
    }

    /// Class B, the whole table: every static row answers its activation
    /// with the notice that names its real mechanism.
    ///
    /// **Swept rather than tabulated, and that is this package's correction.**
    /// The table used to be twenty-five `(card, slot, notice)` literals, and
    /// the write-back moved almost every slot number in it: a row that became
    /// live simply left the table, and one that stayed static kept whatever
    /// row number it used to have. A literal table cannot notice either. This
    /// walks every `RowKind::Note` the screen actually builds, so a notice row
    /// added anywhere is covered the moment it exists and a moved one cannot
    /// silently stop being checked.
    #[test]
    fn every_static_row_answers_with_the_mechanism_that_exists() {
        let base = view();
        let mut seen = 0usize;
        for card in 0..CARDS {
            for (slot, label, kind) in rows_of(&base, card) {
                let RowKind::Note(expected) = kind else {
                    continue;
                };
                seen += 1;
                let mut v = base.clone();
                focus(&mut v, card, slot);
                assert_eq!(
                    handle_key(&mut v, press(KeyCode::Enter)),
                    SettingsAction::FocusChanged,
                    "({card},{slot}) {label:?} is a Note row that asked the shell to act"
                );
                assert_eq!(
                    v.notice.as_deref(),
                    Some(expected),
                    "({card},{slot}) {label:?} told the wrong truth"
                );
            }
        }
        assert!(seen > 8, "the sweep found only {seen} static rows");
        // And the notices point at things that exist, not at absences.
        assert!(NOTICE_PERMISSIONS.contains("settings.local.json"));
        assert!(NOTICE_MODEL.contains("/model"));
        assert!(NOTICE_PROVIDER_KEYS.contains("credentials.json"));
        // The three claims the docs pass found false, pinned where they can go
        // red. Each names the thing that landed, and none of the three says
        // the facility is missing.
        assert!(
            NOTICE_KEYS.contains("~/.emma/keybindings.json"),
            "NOTICE_KEYS must name the file K1 landed, not deny it exists"
        );
        assert!(!NOTICE_KEYS.contains("no keymap file"));
        assert!(
            notice_font_scope(base.terminal.as_ref()).contains("Terminal.app"),
            "the font notice must name the terminal it is looking at"
        );
        // **The defect the docs pass found, pinned.** The sentence used to be
        // a constant naming Terminal.app whatever it was running on, which is
        // false on Windows and was false the moment `termfont` grew its
        // Windows arm. On a terminal with no font control it must say what is
        // missing and must not name a terminal it cannot drive.
        let windows = termfont::Terminal {
            name: termfont::WINDOWS_TERMINAL.into(),
            control: termfont::Control::None,
        };
        let said = notice_font_scope(Some(&windows));
        assert!(
            !said.contains("Terminal.app"),
            "the font notice named a terminal that is not there: {said:?}"
        );
        assert!(
            said.contains(termfont::WINDOWS_TERMINAL) && said.contains("stored"),
            "the notice must name this terminal and say the value is kept: {said:?}"
        );
        assert!(
            !notice_font_scope(None).contains("Terminal.app"),
            "the font notice must not name a terminal it has not read"
        );
        // And the permission card does not imply enforcement it does not
        // have — `CLAUDE.md` calls that a defect in its own right.
        assert!(NOTICE_PERMISSIONS_GATE.contains("never a sandbox"));
    }

    /// **The permission card writes exactly one kind of rule, and reads the
    /// rest.** The security constraint as it stands after the owner's ruling,
    /// asserted where it can go red.
    ///
    /// This replaces `no_key_on_the_permission_card_asks_the_shell_to_write`,
    /// which asserted that *no* row crossed the seam. That test was correct
    /// while it was the guarantee and would now be a false receipt: it would
    /// have to be deleted or weakened to a tautology. What is left of the
    /// guarantee, and what this holds instead, is the split — a tool row asks
    /// for a bare-name rule and nothing else on the card asks for anything,
    /// in every state the card has.
    #[test]
    fn only_the_tool_rows_of_the_permission_card_write_and_only_bare_names() {
        let states: [SettingsView; 3] = [
            view(),
            SettingsView {
                perms: Vec::new(),
                ..view()
            },
            SettingsView {
                perms_read: false,
                ..view()
            },
        ];
        for (i, base) in states.into_iter().enumerate() {
            let rows = rows_of(&base, 5);
            assert!(!rows.is_empty(), "state {i} drew no rows at all");
            let mut writers = 0usize;
            for (slot, label, kind) in rows {
                for code in [KeyCode::Enter, KeyCode::Left, KeyCode::Right] {
                    let mut v = base.clone();
                    focus(&mut v, 5, slot);
                    let action = handle_key(&mut v, press(code));
                    match kind {
                        RowKind::ToolPermission(tool) => {
                            assert!(
                                matches!(action, SettingsAction::ToolPolicy(t, _) if t == tool),
                                "state {i} {label:?} answered {code:?} with {action:?}"
                            );
                        }
                        _ => assert_eq!(
                            action,
                            SettingsAction::FocusChanged,
                            "state {i} row {label:?} asked the shell to act on {code:?}"
                        ),
                    }
                }
                if matches!(kind, RowKind::ToolPermission(_)) {
                    writers += 1;
                }
            }
            assert_eq!(
                writers,
                crate::runctl::ALL_TOOLS.len(),
                "state {i} drew a tool list that is not the registry's"
            );

            // **RULES IN FORCE keeps the original guarantee whole.** That
            // card shows the merge of this project's settings.local.json with
            // the spine file beside the harness — a document this screen must
            // not write at all, and one whose deny outranks a local allow. No
            // key on it may cross the seam, in any of the card's states.
            let rules = rows_of(&base, 9);
            assert!(!rules.is_empty(), "state {i} drew no rules card");
            for (slot, label, _) in rules {
                for code in [KeyCode::Enter, KeyCode::Left, KeyCode::Right] {
                    let mut v = base.clone();
                    focus(&mut v, 9, slot);
                    assert_eq!(
                        handle_key(&mut v, press(code)),
                        SettingsAction::FocusChanged,
                        "state {i} rules row {label:?} asked the shell to act on {code:?}"
                    );
                }
            }
        }
    }

    /// The three states walk in a ring, and `Ask` is the absence rather than a
    /// fourth word.
    #[test]
    fn a_tool_row_walks_ask_allow_deny_and_comes_back() {
        let mut v = view();
        v.tools = Vec::new();
        let slot = slot_of(&v, 5, RowKind::ToolPermission("Bash"));
        let press_right = |v: &mut SettingsView| {
            focus(v, 5, slot);
            handle_key(v, press(KeyCode::Right))
        };
        assert_eq!(
            press_right(&mut v),
            SettingsAction::ToolPolicy("Bash", ToolState::Allow),
            "a tool with no rule starts at Ask"
        );
        v.tools = vec![("Bash".into(), ToolState::Allow)];
        assert_eq!(
            press_right(&mut v),
            SettingsAction::ToolPolicy("Bash", ToolState::Deny)
        );
        v.tools = vec![("Bash".into(), ToolState::Deny)];
        assert_eq!(
            press_right(&mut v),
            SettingsAction::ToolPolicy("Bash", ToolState::Ask),
            "Deny must step back to Ask, which is the absence of a rule"
        );
    }

    /// **A card taller than its cell is windowed, and the window keeps the
    /// slot numbers.** Twenty-seven tool rows in a grid cell that holds six:
    /// what changes is which are painted, never what a slot means, or a click
    /// recorded at paint would focus a different row from the one under the
    /// pointer.
    #[test]
    fn the_tool_card_windows_its_rows_without_renumbering_them() {
        let v = view();
        let cards = cards(&v);
        let tools = &cards[5];
        assert_eq!(tools.window, Some(TOOL_WINDOW));
        assert!(
            tools.kv_count() > TOOL_WINDOW * 2,
            "the card is not long enough for this test to mean anything"
        );
        assert!(
            tools.height() < cards[4].height() + 4,
            "a windowed card must not tower over the one beside it"
        );
        // The window follows the focus and stops at the end of the list.
        assert_eq!(tools.offset(None), 0);
        assert_eq!(tools.offset(Some(0)), 0);
        assert_eq!(tools.offset(Some(10)), 10 - TOOL_WINDOW / 2);
        let last = tools.kv_count() - TOOL_WINDOW;
        assert_eq!(tools.offset(Some(tools.kv_count() - 1)), last);
        // And what a slot means does not move with it: the twentieth tool row
        // is still slot 19 when the window has scrolled to show it.
        let twentieth = crate::runctl::ALL_TOOLS[19];
        assert_eq!(
            slot_of(&v, 5, RowKind::ToolPermission(twentieth)),
            19,
            "the window renumbered a row"
        );
        // And the paint agrees. **This is the half that can go wrong
        // silently**: the rects are recorded from the same arithmetic that
        // painted, so a scrolled window that renumbered as it skipped would
        // record slot 0 for the first *visible* row, and a click on it would
        // focus the first row of the whole list instead. Nothing on screen
        // would look wrong.
        let mut focused = v.clone();
        focused.focus = Some((5, 19));
        let hits = hits_at(&focused, 161, 95);
        let painted: Vec<usize> = hits
            .controls
            .iter()
            .filter_map(|(_, h)| match h {
                Hit::Row(5, slot) => Some(*slot),
                _ => None,
            })
            .collect();
        let first = 19 - TOOL_WINDOW / 2;
        assert_eq!(
            painted,
            (first..first + TOOL_WINDOW).collect::<Vec<_>>(),
            "the painted rects do not carry the card's own slot numbers"
        );
    }

    /// The card shows the rules that are really in force, and says which file
    /// a new one would be written to.
    ///
    /// **Differential**, for the reason A14 is: a compiled-in verdict is
    /// identical on both screens, and a single-render `contains` cannot see
    /// that at all. Two projects, two rule sets, and neither may show the
    /// other's.
    #[test]
    fn the_permission_card_shows_this_projects_rules_and_not_another_s() {
        let with = |rule: &str, verdict: &'static str, file: &str| SettingsView {
            perms: vec![PermRow {
                rule: rule.into(),
                verdict,
            }],
            perms_file: Some(file.into()),
            perms_read: true,
            ..view()
        };
        let a = with(
            "Bash(only-in-project-a *)",
            "allow",
            "/a/settings.local.json",
        );
        let b = with(
            "WebFetch(domain:only-in-project-b.test)",
            "deny",
            "/b/settings.local.json",
        );
        let a_text = draw(&a, 200, 95).join("\n");
        let b_text = draw(&b, 200, 95).join("\n");

        assert!(a_text.contains("only-in-project-a"), "{a_text}");
        assert!(a_text.contains("/a/settings.local.json"), "{a_text}");
        assert!(
            !a_text.contains("only-in-project-b"),
            "the card shows a rule belonging to a different project — it is a \
             constant, not this project's:\n{a_text}"
        );
        assert!(
            b_text.contains("only-in-project-b"),
            "the card did not change when the project did:\n{b_text}"
        );
        assert!(
            !b_text.contains("only-in-project-a"),
            "the card shows a rule belonging to a different project:\n{b_text}"
        );
        // …and the verdict is the rule's own, not a fixed word beside it.
        assert!(
            a_text.contains("allow") && !a_text.contains("deny"),
            "{a_text}"
        );
        assert!(
            b_text.contains("deny") && !b_text.contains("allow"),
            "{b_text}"
        );
    }

    /// A project with no rules says `none`; a screen that never looked says
    /// `not read`. Two different facts, drawn differently, and neither is six
    /// invented verdicts.
    #[test]
    fn no_rules_and_never_looked_are_different_answers() {
        let none = draw(
            &SettingsView {
                perms: Vec::new(),
                ..view()
            },
            200,
            95,
        )
        .join("\n");
        assert!(none.contains("none"), "{none}");
        assert!(none.contains("settings.local.json"), "{none}");

        let unread = draw(
            &SettingsView {
                perms_read: false,
                perms: Vec::new(),
                perms_file: None,
                ..view()
            },
            200,
            95,
        )
        .join("\n");
        assert!(unread.contains("not read"), "{unread}");
    }

    /// The cap names itself and the loss, rather than truncating in silence.
    #[test]
    fn more_rules_than_fit_are_counted_rather_than_dropped_quietly() {
        let v = SettingsView {
            perms: (0..PERM_ROWS_SHOWN + 3)
                .map(|i| PermRow {
                    rule: format!("Bash(rule-{i} *)"),
                    verdict: "allow",
                })
                .collect(),
            ..view()
        };
        let text = draw(&v, 200, 120).join("\n");
        assert!(text.contains("3 more"), "the cap is silent:\n{text}");
        assert!(text.contains("rule-0"), "{text}");
        assert!(!text.contains("rule-10"), "the cap did not hold:\n{text}");
    }

    /// The context card carries this run's numbers, and says `not set` before
    /// a call has reported one. Differential, for A14's reason.
    #[test]
    fn the_context_card_shows_this_runs_numbers_and_not_anothers() {
        let with = |used: i64, cap: i64, budget: i64| SettingsView {
            context: Some((used, cap)),
            goal_budget: Some(budget),
            ..view()
        };
        let a = draw(&with(11_111, 222_222, 333_333), 200, 95).join("\n");
        let b = draw(&with(44_444, 555_555, 666_666), 200, 95).join("\n");
        for (mine, theirs) in [
            ("11111", "44444"),
            ("222222", "555555"),
            ("333333", "666666"),
        ] {
            assert!(a.contains(mine), "{mine} missing:\n{a}");
            assert!(
                !a.contains(theirs),
                "{theirs} belongs to another run — the row is a constant:\n{a}"
            );
            assert!(
                b.contains(theirs),
                "the row did not change with the run:\n{b}"
            );
        }

        // And before any call, absent rather than a plausible figure.
        let cold = draw(
            &SettingsView {
                context: None,
                goal_budget: None,
                ..view()
            },
            200,
            95,
        )
        .join("\n");
        assert!(cold.contains("not set"), "{cold}");
        assert!(!cold.contains("120000"), "{cold}");
    }

    /// Class A: Prune History asks for the opposite of what is shown, both
    /// directions. The row the ruling's "even if they were not before" half
    /// added.
    #[test]
    fn the_prune_toggle_asks_for_the_flip() {
        let mut v = view();
        let slot = slot_of(&v, 4, RowKind::PruneToggle);
        focus(&mut v, 4, slot);
        assert_eq!(
            handle_key(&mut v, press(KeyCode::Enter)),
            SettingsAction::PruneHistory(true)
        );
        v.prune_on = true;
        focus(&mut v, 4, slot);
        assert_eq!(
            handle_key(&mut v, press(KeyCode::Left)),
            SettingsAction::PruneHistory(false)
        );
        // …and it is the *shown* state that is flipped, so the row and the key
        // cannot disagree about what On means.
        assert!(draw(&v, 130, 60).join("\n").contains("Prune History"));
    }

    /// **The provider cycler writes the next run's provider and never
    /// pretends the session switched.**
    ///
    /// This is the row that used to answer with a notice, and the reversal is
    /// the whole point of the row: the file moves now, the bound session does
    /// not, and the value shows both names until they agree. The old test
    /// asserted the notice; asserting it now would pin the page to the
    /// behaviour this package replaced.
    #[test]
    fn the_provider_cycler_writes_the_next_run_and_shows_both_names() {
        let mut v = view();
        v.provider = "ollama".into();
        v.provider_saved = "ollama".into();
        focus(&mut v, 0, 0);
        let action = handle_key(&mut v, press(KeyCode::Right));
        let SettingsAction::Provider(next) = action else {
            panic!("the row must ask the shell to write: {action:?}");
        };
        assert_ne!(next, "ollama", "the chevron must step off the current name");
        assert_eq!(
            v.provider, "ollama",
            "the bound provider must not move from this page"
        );
        // The shell is what sets `provider_saved`; with the two disagreeing,
        // the row says so rather than showing one of them.
        v.provider_saved = next.to_string();
        let text = draw(&v, 200, 95).join("\n");
        assert!(text.contains("(running)"), "{text}");
        assert!(text.contains("(next run)"), "{text}");
        // And the ladder steps from the saved name, not the running one, or a
        // second press would walk back onto the booted provider.
        focus(&mut v, 0, 0);
        assert_eq!(
            handle_key(&mut v, press(KeyCode::Left)),
            SettingsAction::Provider("ollama"),
            "the ladder must walk from where the last press left it"
        );
    }

    /// A refused value leaves the screen exactly as it was — the pure half of
    /// the guarantee the write-back's own tests check on disk.
    ///
    /// The ladders are what the arrows produce, so none of these three is
    /// reachable by pressing a key; each is the *other* door — a settings file
    /// somebody hand-edited, or a view a caller built. A ladder that snapped
    /// an unknown value to its first rung would silently rewrite that choice
    /// on the first arrow press.
    #[test]
    fn a_value_the_ladder_does_not_hold_is_stepped_from_and_never_snapped() {
        // An accent the palette has no name for: the chevrons step back onto
        // the five-role ladder rather than refusing to move at all.
        let mut v = view();
        v.accent = "cube:81".into();
        let slot = slot_of(&v, 2, RowKind::AccentCycle);
        focus(&mut v, 2, slot);
        assert_eq!(
            handle_key(&mut v, press(KeyCode::Right)),
            SettingsAction::Accent(super::super::palette::ACCENTS[1].name.to_string()),
            "an off-ladder accent must step onto the ladder, not stay put"
        );
        // A font size nobody could have cycled to: stepped from, and clamped
        // by `termfont` rather than by this page.
        let mut v = view();
        v.font_size = 999;
        let slot = slot_of(&v, 2, RowKind::FontSizeStep);
        focus(&mut v, 2, slot);
        assert_eq!(
            handle_key(&mut v, press(KeyCode::Right)),
            SettingsAction::FontSize(termfont::MAX_SIZE)
        );
        // A retention typed into settings.json by hand lands on the nearest
        // rung at or above it, and steps from there.
        let mut v = view();
        v.memory_retention = 45;
        let slot = slot_of(&v, 4, RowKind::MemoryRetention);
        focus(&mut v, 4, slot);
        assert_eq!(
            handle_key(&mut v, press(KeyCode::Right)),
            SettingsAction::Retention(365),
            "45 sits between 30 and 90; the next rung up from 90 is 365"
        );
    }

    /// **The temperature ladder can say "the host decides" and can say zero,
    /// and they are different rungs.**
    ///
    /// The rung is hundredths, not an `f64`, so the action is `Eq` and a test
    /// can compare it exactly rather than with an epsilon. A writer that
    /// collapsed `None` and `Some(0)` would pin every provider to greedy
    /// decoding while the row said the host was deciding.
    #[test]
    fn host_default_and_a_chosen_zero_are_two_rungs_not_one() {
        let mut v = view();
        let slot = slot_of(&v, 0, RowKind::TemperatureCycle);
        v.sampling_temperature = None;
        focus(&mut v, 0, slot);
        assert_eq!(
            handle_key(&mut v, press(KeyCode::Right)),
            SettingsAction::Temperature(Some(0)),
            "the rung after Host default is a chosen zero"
        );
        v.sampling_temperature = Some(0.0);
        focus(&mut v, 0, slot);
        assert_eq!(
            handle_key(&mut v, press(KeyCode::Left)),
            SettingsAction::Temperature(None),
            "and stepping back from it removes the key"
        );
        assert_eq!(temperature_label(None), "Host default");
        assert_eq!(temperature_label(Some(0.0)), "0.00");
    }

    /// The font rows say `(stored)` where there is no terminal to ask, and
    /// drop the word where there is — so a value that really was asked for
    /// and one that was only kept are never drawn the same.
    #[test]
    fn the_font_rows_say_when_nothing_was_asked_of_the_terminal() {
        let mut v = view();
        v.font_family = "Menlo".into();
        v.font_size = 14;
        v.terminal = Some(termfont::Terminal {
            name: termfont::WINDOWS_TERMINAL.into(),
            control: termfont::Control::None,
        });
        assert_eq!(font_family_label(&v), "Menlo (stored)");
        assert_eq!(font_size_label(&v), "14 pt (stored)");
        v.terminal = Some(termfont::Terminal {
            name: "Terminal.app".into(),
            control: termfont::Control::AppleScript,
        });
        assert_eq!(font_family_label(&v), "Menlo");
        assert_eq!(font_size_label(&v), "14 pt");
        // And a view nobody filled in does not claim a terminal it never read.
        v.terminal = None;
        assert_eq!(font_family_label(&v), "Menlo (stored)");
    }

    // -- the rendered notice and the button faces ----------------------------

    /// The notice paints on the head's fifth row, under the rule.
    #[test]
    fn the_notice_renders_under_the_rule() {
        let mut v = view();
        v.notice = Some(NOTICE_PERMISSIONS.to_string());
        let rows = draw(&v, 161, 75);
        assert!(
            rows[4].contains("settings.local.json"),
            "notice missing: {:?}",
            rows[4]
        );
    }

    /// The Test Connection button face follows the state: honest `Test`
    /// before anyone measured, `OK`/`FAIL` after.
    #[test]
    fn the_test_button_face_follows_the_state() {
        let mut v = view();
        v.test = TestState::Idle;
        assert!(draw(&v, 130, 60).join("\n").contains("[ Test ]"));
        v.test = TestState::Fail;
        assert!(draw(&v, 130, 60).join("\n").contains("[ FAIL ]"));
    }

    // -- the language servers card (Class B, and honest about `found`) -------

    /// The two dimensions are independent and both are always printed. `Off,
    /// found` is a server you have that Emma will not start; `On, absent` is
    /// the opposite, and it is the one that would read as working if the row
    /// printed only the enabled half.
    #[test]
    fn a_language_row_prints_enabled_and_found_separately() {
        let row = |enabled, found| LspRow {
            label: "Rust".into(),
            key: "rust".into(),
            enabled,
            found,
            network: false,
        };
        assert_eq!(lsp_value(&row(true, LspFound::Found)), "On, found");
        assert_eq!(lsp_value(&row(true, LspFound::Absent)), "On, absent");
        assert_eq!(
            lsp_value(&row(true, LspFound::Needs("dotnet".into()))),
            "On, needs dotnet"
        );
        assert_eq!(lsp_value(&row(false, LspFound::Found)), "Off, found");
        assert_eq!(lsp_value(&row(false, LspFound::Absent)), "Off, absent");
    }

    /// The whole point of the card, on the buffer: an enabled language with
    /// nothing on disk says so on its own row, and never just `On`.
    #[test]
    fn an_enabled_language_with_nothing_on_disk_says_absent() {
        let text = draw(&view(), 161, 95).join("\n");
        assert!(text.contains("9. LANGUAGE SERVERS"), "the card is missing");
        let rust = draw(&view(), 161, 95)
            .into_iter()
            .find(|r| r.contains("Rust") && r.contains("On,"))
            .expect("no Rust row");
        assert!(rust.contains("On, absent"), "Rust row read {rust:?}");
        assert!(text.contains("Off, found"), "a disabled but present server");
        assert!(text.contains("needs pwsh"), "a missing launcher is named");
    }

    /// The summary counts only what is both enabled and on disk. The fixture
    /// has four enabled and one of those found.
    #[test]
    fn the_found_summary_counts_only_enabled_languages() {
        let text = draw(&view(), 161, 95).join("\n");
        assert!(text.contains("1 of 4 on"), "summary missing from:\n{text}");
    }

    /// Class B, every row: activation changes nothing on disk and answers with
    /// the notice that names the real mechanism. The three network-flagged
    /// languages carry the notice that says why they are off by default.
    #[test]
    fn every_language_row_is_class_b_and_names_the_real_mechanism() {
        let v = view();
        let rows = v.lsp.len() + 1; // the languages plus Found On Disk
        for slot in 0..rows {
            let mut v = view();
            v.focus = Some((8, slot));
            assert_eq!(
                handle_key(&mut v, press(KeyCode::Enter)),
                SettingsAction::FocusChanged,
                "row {slot} asked the shell to act"
            );
            let notice = v.notice.clone().unwrap_or_default();
            assert!(
                notice == NOTICE_LSP || notice == NOTICE_LSP_NETWORK || notice == NOTICE_LSP_FOUND,
                "row {slot} answered {notice:?}"
            );
        }
        let mut v = view();
        v.focus = Some((8, 4)); // Terraform, the first network-flagged one
        handle_key(&mut v, press(KeyCode::Enter));
        assert_eq!(v.notice.as_deref(), Some(NOTICE_LSP_NETWORK));
        let mut v = view();
        v.focus = Some((8, 1)); // Bash
        handle_key(&mut v, press(KeyCode::Enter));
        assert_eq!(v.notice.as_deref(), Some(NOTICE_LSP));
    }

    /// `found` is a file on disk, not a working server, and the row that
    /// carries the count is where that is said out loud.
    #[test]
    fn the_found_row_says_what_found_means() {
        let mut v = view();
        v.focus = Some((8, 7));
        handle_key(&mut v, press(KeyCode::Enter));
        let notice = v.notice.clone().unwrap();
        assert_eq!(notice, NOTICE_LSP_FOUND);
        assert!(notice.contains("on disk"), "{notice}");
        assert!(notice.contains("starts no process"), "{notice}");
    }

    /// Keys this build does not know are shown, not dropped.
    #[test]
    fn unknown_enabled_keys_get_their_own_row() {
        let mut v = view();
        v.lsp_unknown = vec!["cobol".into()];
        let text = draw(&v, 161, 95).join("\n");
        assert!(text.contains("Unknown Keys"), "no row for an unknown key");
        assert!(text.contains("cobol"));
    }

    /// A view nobody has filled says so rather than printing seven confident
    /// `Off`s that no file was read for.
    #[test]
    fn an_unread_language_list_says_not_read() {
        let v = SettingsView {
            version: "v0".into(),
            ..SettingsView::default()
        };
        let text = draw(&v, 161, 95).join("\n");
        assert!(
            text.contains("not read"),
            "guessed instead of saying:\n{text}"
        );
    }

    // -- the mouse (rects at paint, pure hit-test) ---------------------------

    fn hits_at(v: &SettingsView, w: u16, h: u16) -> Hits {
        let area = Rect::new(0, 0, w, h);
        let mut buf = Buffer::empty(area);
        render_hits(area, &mut buf, v, &skin())
    }

    /// Every Kv row on every card records a rect: 6+6+5+2+5+4+4+3 = 35 for
    /// the mock's eight (card 6 being the fixture's two rules plus its file
    /// and provenance rows), plus the seven languages and the Found On Disk
    /// summary.
    #[test]
    fn every_row_records_a_click_rect() {
        let v = view();
        let hits = hits_at(&v, 161, 95);
        let rows: Vec<_> = hits
            .controls
            .iter()
            .filter(|(_, h)| matches!(h, Hit::Row(..)))
            .collect();
        // One rect per Kv row that was **painted**, which is not the same as
        // one per Kv row the cards hold: TOOL PERMISSIONS is windowed, and a
        // row outside the window is deliberately not clickable — a rect
        // recorded for a row nobody can see is a click that lands on nothing.
        // Counted from the cards rather than written out, because the number
        // moves whenever a row is added and a literal would then be asserting
        // the old screen.
        let painted: usize = (0..CARDS)
            .map(|c| {
                let card = &cards(&v)[c];
                card.kv_count().min(card.window.unwrap_or(usize::MAX))
            })
            .sum();
        assert_eq!(rows.len(), painted, "one Row rect per painted Kv row");
        assert!(
            painted < (0..CARDS).map(|c| cards(&v)[c].kv_count()).sum(),
            "the windowed card painted everything, so this proves nothing"
        );
        // The cyclers report chevron rects; the buttons report Act rects.
        assert!(hits.controls.iter().any(|(_, h)| *h == Hit::Prev(2, 0)));
        assert!(hits.controls.iter().any(|(_, h)| *h == Hit::Next(2, 0)));
        assert!(hits.controls.iter().any(|(_, h)| *h == Hit::Act(7, 2)));
    }

    /// A press inside a recorded rect resolves to its control, the most
    /// specific rect wins, and a press on empty ground resolves to nothing.
    #[test]
    fn the_hit_test_is_the_paint_read_back() {
        let v = view();
        let hits = hits_at(&v, 161, 75);
        let next = hits
            .controls
            .iter()
            .find(|(_, h)| *h == Hit::Next(2, 0))
            .expect("theme › chevron recorded");
        assert_eq!(hit(&hits, next.0.x, next.0.y), Some(Hit::Next(2, 0)));
        let row = hits
            .controls
            .iter()
            .find(|(_, h)| *h == Hit::Row(2, 0))
            .expect("theme row recorded");
        assert_eq!(
            hit(&hits, row.0.x, row.0.y),
            Some(Hit::Row(2, 0)),
            "the label side of the row focuses"
        );
        assert_eq!(hit(&hits, 0, 0), None, "empty ground swallows nothing");
    }
}
