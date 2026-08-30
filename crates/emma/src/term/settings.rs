//! The Settings screen: the owner's mock, cell for cell — and live.
//!
//! The mock is the acceptance criterion (`notes/design-settings-tui.md`): a
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
//! (`notes/design/settings-wiring.md`): every row ends in exactly one of three
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
    /// The provider in force, by its registry name (`anthropic`, `ollama`).
    /// Displayed with its first letter raised; cycling it is a notice, not a
    /// switch — see [`NOTICE_PROVIDER`].
    pub provider: String,
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
pub const CARDS: usize = 9;

// endregion: State

// region: The honest notices
// ---------------------------------------------------------------------------
// The honest notices
//
// One constant per claim, so a test asserts the exact sentence and the
// sentence names the mechanism that really exists. Never "not implemented"
// alone.
// ---------------------------------------------------------------------------

pub const NOTICE_PROVIDER: &str = "Provider is chosen per run: `emma set-provider <name>` stores \
     its key and selects it; /model --save picks its model";
pub const NOTICE_MODEL: &str =
    "/model lists this provider's models; /model <id> switches this session, --save remembers it";
pub const NOTICE_SAMPLING: &str =
    "Sampling is the provider's default today; no temperature or output-cap setting exists yet";
pub const NOTICE_STREAMING: &str =
    "Streaming is always on; the transcript renders deltas as they arrive";
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
pub const NOTICE_THEME_OWNED: &str =
    "Colours come from the theme: cycle the Theme row, or /theme <name>";
pub const NOTICE_FONT: &str =
    "Fonts belong to your terminal emulator; Emma cannot set one from inside it";
pub const NOTICE_STATUSBAR: &str = "The status bar follows the statusLine block of the \
     project's settings.json, read by the harness";
pub const NOTICE_KEYS: &str =
    "Keybindings are fixed today; the sidebar's QUICK HELP lists them, and no keymap file exists";
pub const NOTICE_MEMORY_STAGES: &str = "Memory pages live under .emma/memory (Alt+m manages \
     them); retention, auto-recall and scope need the retrieval stage (M2)";
pub const NOTICE_PRUNE: &str = "Prune History drops superseded tool results from what is sent \
     back to the model; off unless set, because it changes what the model is shown";
pub const NOTICE_PERMISSIONS: &str = "Permission rules live in settings.local.json. [r] or [t] \
     at an approval prompt writes one, and it is honoured by every later run in this project";
/// Why the card is read-only, said on the card rather than in a design note.
///
/// **This is the security constraint made visible.** A row that could edit a
/// permission would be a settings route into an execution decision, and the
/// only reason it is safe today is that no key on this page writes one.
pub const NOTICE_PERMISSIONS_READONLY: &str = "This card reads the rules and never writes one: a \
     permission is granted at the approval prompt, where the call that wants it is on screen";
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
pub const NOTICE_RESET_CONFIRM: &str =
    "Reset clears theme and memory capture to defaults — Enter again confirms, Esc cancels";
pub const NOTICE_RESET_CANCELLED: &str = "Reset cancelled — nothing changed";

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
    /// Enter/←/→ flip `memory_capture` in settings.json.
    MemoryToggle,
    /// Enter/←/→ flip `prune_history` in settings.json.
    PruneToggle,
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
/// Every row is one of the module doc's three states. Live and editable:
/// Theme, Enable Memory, Prune History, Test Connection, Save, Export, Reset.
/// Live and read-only: Provider, Model, Streaming, the two context numbers,
/// Telemetry, Working Directory, and every row of cards 6 and 9. Absent —
/// `Value::Absent`, dim: Temperature, Max Output Tokens, Response Budget, the
/// appearance rows below Theme, both keybinding rows, the three memory rows
/// below Enable Memory, Environment and Log Level.
fn cards(s: &SettingsView) -> Vec<Card> {
    use RowKind::Note;
    use Value::{Absent, Button, Cycler, Plain};
    vec![
        Card {
            header: "1. MODEL PROVIDER",
            rows: vec![
                // Live and read-only, and the cycler dress is gone with the
                // pretence: `emma_llm::known()` has exactly one entry in this
                // build, so a chevron would step from anthropic to anthropic.
                kv(
                    "Provider",
                    Plain(title_case(&s.provider)),
                    Note(NOTICE_PROVIDER),
                ),
                kv("Model", Plain(s.model.clone()), Note(NOTICE_MODEL)),
                kv("Temperature", Absent("n/a".into()), Note(NOTICE_SAMPLING)),
                kv(
                    "Max Output Tokens",
                    Absent("n/a".into()),
                    Note(NOTICE_SAMPLING),
                ),
                // A fact about the build rather than a setting: there is no
                // non-streaming path to switch to.
                kv("Streaming", Plain("On".into()), Note(NOTICE_STREAMING)),
                CardRow::Divider,
                kv(
                    "Test Connection",
                    Button(test_label(s.test).into()),
                    RowKind::TestConnection,
                ),
            ],
        },
        Card {
            header: "2. CONTEXT LIMITS",
            rows: vec![
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
        },
        Card {
            header: "3. APPEARANCE",
            rows: vec![
                kv("Theme", Cycler(title_case(&s.theme)), RowKind::ThemeCycle),
                kv(
                    "Accent Color",
                    Absent("n/a".into()),
                    Note(NOTICE_THEME_OWNED),
                ),
                kv("Font Family", Absent("n/a".into()), Note(NOTICE_FONT)),
                kv("Font Size", Absent("n/a".into()), Note(NOTICE_FONT)),
                kv("Status Bar", Absent("n/a".into()), Note(NOTICE_STATUSBAR)),
                CardRow::Desc("Customize Emma's look and feel.".into()),
            ],
        },
        Card {
            header: "4. KEYBINDINGS",
            rows: vec![
                // Both dresses dropped. A cycler with one preset and a button
                // that opens nothing are the `QUICK HELP` defect one level
                // down: an affordance no key answers.
                kv(
                    "Keybinding Preset",
                    Absent("n/a".into()),
                    Note(NOTICE_KEYS),
                ),
                kv("Edit Keybindings", Absent("n/a".into()), Note(NOTICE_KEYS)),
                CardRow::Desc("Keys are fixed; the sidebar's QUICK HELP lists them.".into()),
            ],
        },
        Card {
            header: "5. MEMORY PREFERENCES",
            rows: vec![
                kv(
                    "Enable Memory",
                    Plain(if s.memory_on { "On" } else { "Off" }.into()),
                    RowKind::MemoryToggle,
                ),
                // New here, and the ruling's "even if they were not before"
                // half: `prune_history` is personal settings with no UI until
                // now.
                kv(
                    "Prune History",
                    Plain(if s.prune_on { "On" } else { "Off" }.into()),
                    RowKind::PruneToggle,
                ),
                kv(
                    "Memory Retention",
                    Absent("n/a".into()),
                    Note(NOTICE_MEMORY_STAGES),
                ),
                kv(
                    "Auto-Recall",
                    Absent("n/a".into()),
                    Note(NOTICE_MEMORY_STAGES),
                ),
                kv(
                    "Memory Scope",
                    Absent("n/a".into()),
                    Note(NOTICE_MEMORY_STAGES),
                ),
                CardRow::Desc("Control how Emma remembers information.".into()),
            ],
        },
        Card {
            header: "6. TOOL PERMISSIONS",
            rows: perm_rows(s),
        },
        Card {
            header: "7. ENVIRONMENT",
            rows: vec![
                kv("Working Directory", Plain(s.cwd.clone()), Note(NOTICE_CWD)),
                kv("Environment", Absent("n/a".into()), Note(NOTICE_ENV)),
                kv("Log Level", Absent("n/a".into()), Note(NOTICE_ENV)),
                // Not a switch that happens to be off: there is no sender.
                kv("Telemetry", Plain("none sent".into()), Note(NOTICE_TELEMETRY)),
                CardRow::Desc("Environment and runtime configuration.".into()),
            ],
        },
        Card {
            header: "8. SAVE & RESET",
            rows: vec![
                kv("Save Settings", Button("Save Now".into()), RowKind::Save),
                kv("Export Settings", Button("Export".into()), RowKind::Export),
                CardRow::Divider,
                kv("Reset to Defaults", Button("Reset".into()), RowKind::Reset),
                CardRow::Desc("Reset will restore all settings to defaults.".into()),
            ],
        },
        Card {
            header: "9. LANGUAGE SERVERS",
            rows: lsp_rows(s),
        },
    ]
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

/// The TOOL PERMISSIONS card's rows: the real rules, in the order they are
/// consulted, and the file a new one would be written to.
///
/// **Every row is [`RowKind::Note`], and that is the security constraint
/// rather than an unfinished half.** Editing a permission from here would
/// write `settings.local.json` — the same file `[r]` and `[t]` at an approval
/// prompt write — and a settings route into an execution decision is exactly
/// the shape the `another agent` review found. The keyboard is not a model-reachable
/// route today, but the write would be a second door to a decision that has
/// one good door, and the good one has the call that wants the grant on screen
/// beside it. So this card reads and never writes.
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
        "Changed From",
        Plain("the approval prompt".into()),
        Note(NOTICE_PERMISSIONS_READONLY),
    ));
    rows.push(CardRow::Desc(
        "Read-only: a grant is made at the prompt, with the call on screen.".into(),
    ));
    rows
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
    /// Store `memory_capture`: `true` removes the key (absent means on),
    /// `false` writes `false`.
    MemoryCapture(bool),
    /// Store `prune_history`: `false` removes the key (absent means off),
    /// `true` writes `true`. The mirror image of [`Self::MemoryCapture`],
    /// because the two defaults are opposite.
    PruneHistory(bool),
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
        Some(RowKind::ThemeCycle) => {
            v.confirm_reset = false;
            SettingsAction::Theme(theme_step(&v.themes, &v.theme, 1))
        }
        Some(RowKind::MemoryToggle) => {
            v.confirm_reset = false;
            SettingsAction::MemoryCapture(!v.memory_on)
        }
        Some(RowKind::PruneToggle) => {
            v.confirm_reset = false;
            SettingsAction::PruneHistory(!v.prune_on)
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
/// `notes/design/term-hardening-backport.md`; the guarantee predates the TUI
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
            let tallest = pair
                .iter()
                .map(|c| c.rows.len() as u16 + 3) // header + rows + border
                .max()
                .unwrap_or(0);
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
        let tallest = pair
            .iter()
            .map(|c| c.rows.len() as u16 + 3) // header + rows + border
            .max()
            .unwrap_or(0);
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
    let mut slot = 0usize;
    for row in &card.rows {
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
    let mut dresses = card.rows.iter().filter_map(|r| match r {
        CardRow::Kv(_, v, _) => Some(v),
        _ => None,
    });
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
            ..SettingsView::default()
        }
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
            ("Temperature", "n/a"),
            ("Max Context Tokens", "120000"),
            ("Memory Scope", "n/a"),
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
    /// The negative half is the half that matters and the half a `contains`
    /// test cannot express: the provider cycler, the keybinding preset cycler,
    /// the `[ Open ]` button and the six `Ask ›` / `Allow ›` chevrons must
    /// **not** be on the page, because no key changes any of them.
    #[test]
    fn an_edit_affordance_is_drawn_only_where_a_key_answers_it() {
        let all = draw(&view(), 130, 60).join("\n");
        assert!(all.contains("‹ Dracula ›"), "theme cycler missing");
        for b in ["[ OK ]", "[ Save Now ]", "[ Export ]", "[ Reset ]"] {
            assert!(all.contains(b), "button {b} missing");
        }
        for dead in [
            "‹ Ollama ›",
            "‹ Default ›",
            "[ Open ]",
            "Ask ›",
            "Allow ›",
        ] {
            assert!(
                !all.contains(dead),
                "{dead} is drawn as editable and no key changes it"
            );
        }
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
            "30 days",       // Memory Retention
            "Project",       // Memory Scope
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
        for label in ["Environment", "Log Level"] {
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

    /// ↑/↓ move within the focused card and clamp at its edges — the first
    /// card has six rows (Test Connection is the sixth), and ↓ never leaves it.
    #[test]
    fn arrows_move_within_a_card_and_clamp() {
        let mut v = view();
        handle_key(&mut v, press(KeyCode::Tab));
        for _ in 0..9 {
            handle_key(&mut v, press(KeyCode::Down));
        }
        assert_eq!(v.focus, Some((0, 5)), "↓ escaped card 1 or overclamped");
        for _ in 0..9 {
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
        focus(&mut v, 0, 5);
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
    #[test]
    fn every_static_row_answers_with_the_mechanism_that_exists() {
        let table: &[(usize, usize, &str)] = &[
            (0, 0, NOTICE_PROVIDER),
            (0, 1, NOTICE_MODEL),
            (0, 2, NOTICE_SAMPLING),
            (0, 3, NOTICE_SAMPLING),
            (0, 4, NOTICE_STREAMING),
            (1, 0, NOTICE_CONTEXT),
            (1, 2, NOTICE_OUTPUT_CAP),
            (1, 5, NOTICE_CONTEXT),
            (2, 1, NOTICE_THEME_OWNED),
            (2, 2, NOTICE_FONT),
            (2, 3, NOTICE_FONT),
            (2, 4, NOTICE_STATUSBAR),
            (3, 0, NOTICE_KEYS),
            (3, 1, NOTICE_KEYS),
            (4, 2, NOTICE_MEMORY_STAGES),
            (4, 3, NOTICE_MEMORY_STAGES),
            (4, 4, NOTICE_MEMORY_STAGES),
            (5, 0, NOTICE_PERMISSIONS_GATE),
            (5, 1, NOTICE_PERMISSIONS_GATE),
            (5, 2, NOTICE_PERMISSIONS),
            (5, 3, NOTICE_PERMISSIONS_READONLY),
            (6, 0, NOTICE_CWD),
            (6, 1, NOTICE_ENV),
            (6, 2, NOTICE_ENV),
            (6, 3, NOTICE_TELEMETRY),
        ];
        for &(card, slot, expected) in table {
            let mut v = view();
            focus(&mut v, card, slot);
            assert_eq!(
                handle_key(&mut v, press(KeyCode::Enter)),
                SettingsAction::FocusChanged,
                "({card},{slot}) is not a notice row"
            );
            assert_eq!(
                v.notice.as_deref(),
                Some(expected),
                "({card},{slot}) told the wrong truth"
            );
        }
        // And the notices point at things that exist, not at absences.
        assert!(NOTICE_PERMISSIONS.contains("settings.local.json"));
        assert!(NOTICE_PROVIDER.contains("set-provider"));
        assert!(NOTICE_MODEL.contains("/model"));
        assert!(NOTICE_MEMORY_STAGES.contains(".emma/memory"));
        assert!(NOTICE_STATUSBAR.contains("statusLine"));
        // And the permission card does not imply enforcement it does not
        // have — `CLAUDE.md` calls that a defect in its own right.
        assert!(NOTICE_PERMISSIONS_GATE.contains("never a sandbox"));
    }

    /// **No key on this page writes a permission.** The security constraint,
    /// asserted where it can go red: every row of card 6, in every state the
    /// card has, answers Enter and ←/→ with a notice and never with an action
    /// that crosses the seam.
    ///
    /// A row that became editable would return something other than
    /// `FocusChanged` here, so this fails the moment the card grows a write —
    /// which is the only way the failure could be noticed before it shipped.
    #[test]
    fn no_key_on_the_permission_card_asks_the_shell_to_write() {
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
            let n = slots(&base, 5);
            assert!(n > 0, "state {i} drew no rows at all");
            for slot in 0..n {
                for code in [KeyCode::Enter, KeyCode::Left, KeyCode::Right] {
                    let mut v = base.clone();
                    focus(&mut v, 5, slot);
                    assert_eq!(
                        handle_key(&mut v, press(code)),
                        SettingsAction::FocusChanged,
                        "state {i} row {slot} asked the shell to act on {code:?}"
                    );
                }
            }
        }
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
        let a = with("Bash(only-in-project-a *)", "allow", "/a/settings.local.json");
        let b = with("WebFetch(domain:only-in-project-b.test)", "deny", "/b/settings.local.json");
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
        assert!(a_text.contains("allow") && !a_text.contains("deny"), "{a_text}");
        assert!(b_text.contains("deny") && !b_text.contains("allow"), "{b_text}");
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
            assert!(b.contains(theirs), "the row did not change with the run:\n{b}");
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
        focus(&mut v, 4, 1);
        assert_eq!(
            handle_key(&mut v, press(KeyCode::Enter)),
            SettingsAction::PruneHistory(true)
        );
        v.prune_on = true;
        assert_eq!(
            handle_key(&mut v, press(KeyCode::Left)),
            SettingsAction::PruneHistory(false)
        );
        // …and it is the *shown* state that is flipped, so the row and the key
        // cannot disagree about what On means.
        assert!(draw(&v, 130, 60).join("\n").contains("Prune History"));
    }

    /// The provider cycler's ←/→ answer with the honest notice too — the
    /// display never pretends the session switched.
    #[test]
    fn the_provider_cycler_tells_the_truth_instead_of_pretending() {
        let mut v = view();
        focus(&mut v, 0, 0);
        assert_eq!(
            handle_key(&mut v, press(KeyCode::Right)),
            SettingsAction::FocusChanged
        );
        assert_eq!(v.notice.as_deref(), Some(NOTICE_PROVIDER));
        assert_eq!(v.provider, "ollama", "the display must not fake a switch");
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
        let hits = hits_at(&view(), 161, 95);
        let rows: Vec<_> = hits
            .controls
            .iter()
            .filter(|(_, h)| matches!(h, Hit::Row(..)))
            .collect();
        assert_eq!(rows.len(), 43, "one Row rect per Kv row");
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
