//! The full-screen frame: the layout that owns the whole window, and the state
//! that survives between paints.
//!
//! This is stage 2 of the full-screen design — the point of no
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
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;

use crate::harness_state::{self, relative_time};

use super::palette::Role;
use super::render::{fit, Skin};
use super::transcript::{Cap, EntryKind, Transcript};
use super::view::View;
use super::{chat, settings, sidebar, statusbar, transcript};

// region: The layout seam
// ---------------------------------------------------------------------------
// The layout seam
//
// The latch, the carve, and the compose seam moved to `layout.rs` — one
// composition, every window. Re-exported here because this module is where
// the rest of the crate learned those names, and a move should not be a
// rename.
// ---------------------------------------------------------------------------

pub use super::layout::{
    dock_height, hidden, regions, Latch, Regions, SLIM_HEADER_ROWS, SLIM_STATUS_ROWS,
    UNBORDERED_COLS,
};

// endregion: The layout seam

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
    /// The user-tool catalogue as rows, from [`App::set_tools`]. Consulted at
    /// paint time for one thing only: which of the mock's TOOLS rows must read
    /// `n/a` instead of a chord. Empty means nobody has probed a directory yet,
    /// and every row renders as available — which is what an inline run and a
    /// bare `App::new` both want.
    tools: Vec<sidebar::Row>,
    side: sidebar::State,
    /// The column entries are wrapped to: [`chat::message_width`] of the chat
    /// pane, so the wrapper and the painter agree on where the column ends —
    /// the one number the chat module insists both sides derive from it.
    wrap_width: u16,
    /// The chat pane's height at the last layout; less one row, a page.
    chat_height: u16,
    /// Whether the Settings screen owns the main pane. Toggled by `,` on an
    /// empty input box — the key the mock binds to its Settings row.
    settings_open: bool,
    /// The Memory page (plan M0/M1 meeting). `Some` while open; rebuilt from
    /// the wiki on every open so the page never shows a stale store.
    memory: Option<super::memory::MemoryView>,
    /// The Harness dashboard. Same law as memory: read fresh on open.
    harness: Option<super::harness::HarnessView>,
    /// What SESSIONS is a list of, kept from `set_identity` so the list can be
    /// rebuilt later without the shell handing the same three facts in again.
    scope: Option<SessionScope>,
    /// Where the SESSIONS header's `[+]` was on the last paint, or `None` when
    /// the sidebar was collapsed or too narrow to draw one.
    ///
    /// Recorded here for the same reason [`App::chat_rect`] is: the pointer
    /// hit-tests against where the reader saw the control, and the only thing
    /// that knows that is the code that just painted it. It is geometry off
    /// [`Regions::sidebar`] rather than a rectangle `sidebar.rs` hands back —
    /// that module renders and does not answer questions about itself.
    sessions_add: Option<Rect>,
    /// Where the chat pane was on the last paint — the message column, with
    /// the scrollbar's own column already taken out of it. The mouse hit-tests
    /// against this, so a click lands where the reader saw the text.
    chat_rect: Rect,
    /// The scrollbar's column on the last paint, when it drew one.
    scrollbar: Option<u16>,
    /// A live scrollbar drag: how many rows below the thumb's top the button
    /// went down, held for the length of the drag so the thumb stays under
    /// the pointer that picked it up. `None` when no drag is running.
    /// The thumb press that started a drag: the scroll offset at the press
    /// and the track row it landed on. Both, because the drag is anchored:
    /// each move is a delta from the press rather than an absolute mapping of
    /// the pointer onto the track, which is what made the first drag event
    /// lurch the view by hundreds of rows.
    bar_grab: Option<(usize, u16)>,
    /// The bottom-right notice: what a mouse selection just put on the
    /// clipboard, standing in the same slot as `↓ N rows below`.
    ///
    /// ⚠ NO TIMER CLEARS THIS, and none should: chrome that changes without
    /// an event behind it means a repaint loop running for the sake of a
    /// message, on a pane that otherwise only paints when something happens.
    /// It goes on the next thing the reader does — a key, a scroll, another
    /// selection — which is also when they have stopped reading it.
    notice: Option<String>,
    /// The live selection: anchor and head, in pane-relative cells. The
    /// anchor is where the button went down, so a backwards drag is a
    /// backwards drag and not a smaller rectangle.
    selection: Option<(Cell, Cell)>,
    /// The chat pane exactly as it was last painted, one symbol per cell.
    ///
    /// ⚠ WHAT THE READER SAW IS WHAT GETS COPIED, which is why this is kept
    /// at all rather than re-derived from the transcript at copy time. The
    /// rows on screen are wrapped, gutter-shifted and windowed; a second
    /// assembly of them would be a second renderer of the same text, and the
    /// two would drift the first time either side changed — the drift
    /// `chat.rs` refuses for the label column, arriving here from the
    /// clipboard's direction.
    chat_cells: Vec<Vec<String>>,
    /// The wiki root the open page reads from — the cwd `toggle_memory` was
    /// handed, kept so every action can re-open the same store.
    memory_cwd: String,
    /// The repo the open Harness page filters runs to — `toggle_harness`'s
    /// cwd, kept so refresh re-reads the same history.
    harness_cwd: String,
    /// The session directory the harness feeds read. `~/.emma/sessions` in
    /// the product; a tempdir in tests, which is why it is state and not a
    /// constant.
    harness_dir: std::path::PathBuf,
    /// Where the open Harness page's clickable controls were on the last
    /// paint — the `sessions_add` rule: the pointer hit-tests against what
    /// the reader saw, and the paint is what knows where that was.
    harness_hits: super::harness::Hits,
    /// The Settings screen's interaction state. Always present because the
    /// screen itself is a `bool` (`settings_open`); the live fields are
    /// refreshed from disk on every open and from the `View` at paint.
    settings: super::settings::SettingsView,
    /// Where the Settings screen's controls were on the last paint — the
    /// harness's rule, one field per page.
    settings_hits: super::settings::Hits,
    /// The home directory settings.json lives under. Resolved once; a test
    /// points it at a tempdir so no test touches the real file.
    home: Option<std::path::PathBuf>,
    /// Test seam for the Ollama ping: `Some` overrides the `OLLAMA_HOST`
    /// resolution so a test can aim at a listener it owns.
    ollama_host_override: Option<String>,
}

/// Today's civil date in the machine's local zone, for the sidebar calendar.
/// Computed at paint time rather than stored, so a session that runs past
/// midnight moves its mark without a timer.
fn today_local() -> sidebar::Today {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    sidebar::civil_from_secs(secs + crate::harness_state::local_offset_secs())
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
            tools: Vec::new(),
            side: sidebar::State {
                sessions: Vec::new(),
                // TOOLS and QUICK HELP are the sidebar's canonical sections
                // (`sidebar::tool_rows` / `sidebar::quick_help`), rebuilt at
                // paint time because the glyph set is the skin's to know.
                commands: Vec::new(),
                help: Vec::new(),
                today: None,
                collapsed: false,
            },
            wrap_width: chat::message_width(r.chat.width.max(1)),
            chat_height: r.chat.height.max(1),
            settings_open: false,
            memory: None,
            harness: None,
            scope: None,
            sessions_add: None,
            chat_rect: r.chat,
            scrollbar: None,
            bar_grab: None,
            notice: None,
            selection: None,
            chat_cells: Vec::new(),
            memory_cwd: String::new(),
            harness_cwd: String::new(),
            harness_hits: super::harness::Hits::default(),
            settings: super::settings::SettingsView::default(),
            settings_hits: super::settings::Hits::default(),
            home: emma_llm::auth::home_dir(),
            ollama_host_override: None,
            harness_dir: std::env::var_os("HOME")
                .map(|h| std::path::Path::new(&h).join(".emma/sessions"))
                .unwrap_or_default(),
        }
    }

    /// One block of transcript output. Each call is one [`Transcript`] entry —
    /// see the module doc for what that costs and why.
    /// The last assistant turn's source, for the clipboard. See
    /// [`super::transcript::Transcript::last_assistant_source`].
    pub fn last_assistant_source(&self) -> Option<String> {
        self.transcript.last_assistant_source()
    }

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

    /// The Settings screen, on and off. State only: what it shows is
    /// [`settings::render`]'s, and the values it draws are read at paint time.
    pub fn toggle_settings(&mut self) {
        self.settings_open = !self.settings_open;
        // One main-region occupant at a time: opening any screen closes the
        // others, so the dispatch below never has to rank them.
        if self.settings_open {
            self.memory = None;
            self.harness = None;
            // The live rows read disk truth on every open — memory's law.
            // model/cwd/version stay paint-time: the `View` owns them.
            let stored = self
                .home
                .as_deref()
                .map(crate::settings::load)
                .unwrap_or_default();
            self.settings.provider = stored
                .provider
                .clone()
                .unwrap_or_else(|| emma_llm::DEFAULT_PROVIDER.to_string());
            // The *selection*, read where it is written. This tree has no
            // ambient "active theme": `theme::load` resolves the name once at
            // startup and `Palette` is `Copy`, so what is live on screen is
            // whatever `settings.json` said when the process began. The row
            // shows what is selected and the notice says when it takes effect.
            self.settings.theme = stored
                .theme
                .clone()
                .unwrap_or_else(|| super::theme::BUILT_IN.to_string());
            // `None` for the harness root: the app holds a home directory and
            // no project root, so a theme file under `<project>/.emma/themes`
            // is selectable by `/theme <name>` and does not appear in this
            // cycler. Narrower than the command, and honestly so — a row that
            // offered a name it could not step back to would be worse.
            self.settings.themes = super::theme::names(self.home.as_deref(), None);
            self.settings.memory_on = stored.memory.unwrap_or(true);
            // Absent means **off** here, the opposite of `memory` above and
            // deliberately so — `crate::settings::Settings::prune_history`
            // carries the argument.
            self.settings.prune_on = stored.prune_history.unwrap_or(false);
            // The project's permission rules, read where they are written.
            // Same law as the rest of this block: on every open, never cached
            // across one, so a grant made at a prompt since the last look is
            // on the card the next time it is opened.
            let (perms, perms_file, perms_read) = permission_rows();
            self.settings.perms = perms;
            self.settings.perms_file = perms_file;
            self.settings.perms_read = perms_read;
            self.settings.test = super::settings::TestState::Idle;
            self.settings.focus = None;
            self.settings.notice = None;
            self.settings.confirm_reset = false;
            let (rows, unknown) = lsp_rows(&stored);
            self.settings.lsp = rows;
            self.settings.lsp_unknown = unknown;
        }
    }

    /// One key, while the Settings screen is open. Returns whether the page
    /// consumed it; `false` lets the global layer (Alt+…, Ctrl-C) keep the
    /// key. The shell half of the settings seam: the pure half is
    /// [`super::settings::handle_key`], and every action that needs disk or
    /// network lands here, beside the file it touches.
    pub fn settings_key(&mut self, key: ratatui::crossterm::event::KeyEvent) -> bool {
        use super::settings::{handle_key, SettingsAction};
        use ratatui::crossterm::event::{KeyEventKind, KeyModifiers};

        if !self.settings_open {
            return false;
        }
        if key.kind == KeyEventKind::Release
            || key
                .modifiers
                .intersects(KeyModifiers::ALT | KeyModifiers::CONTROL)
        {
            return false;
        }
        match handle_key(&mut self.settings, key) {
            SettingsAction::Close => self.settings_open = false,
            SettingsAction::Theme(name) => self.settings_theme(name),
            SettingsAction::MemoryCapture(on) => self.settings_memory(on),
            SettingsAction::PruneHistory(on) => self.settings_prune(on),
            SettingsAction::Save => self.settings_save(),
            SettingsAction::Export => self.settings_export(),
            SettingsAction::Reset => self.settings_reset(),
            SettingsAction::TestConnection => self.settings_test(),
            SettingsAction::None | SettingsAction::FocusChanged => {}
        }
        // Every plain key belongs to the open page, acted on or not — the
        // memory page's law: nothing else could honestly reach it.
        true
    }

    /// A left-button press while the Settings screen is open. A control is
    /// its key: chevrons are ←/→ and values are Enter, dispatched through
    /// [`Self::settings_key`], so the two paths can never disagree.
    pub fn settings_click(&mut self, col: u16, row: u16) -> bool {
        use super::settings::{hit, Hit};
        use ratatui::crossterm::event::{KeyCode, KeyEvent};
        if !self.settings_open {
            return false;
        }
        match hit(&self.settings_hits, col, row) {
            Some(Hit::Row(c, s)) => {
                self.settings.focus = Some((c, s));
                true
            }
            Some(Hit::Prev(c, s)) => {
                self.settings.focus = Some((c, s));
                self.settings_key(KeyEvent::from(KeyCode::Left))
            }
            Some(Hit::Next(c, s)) => {
                self.settings.focus = Some((c, s));
                self.settings_key(KeyEvent::from(KeyCode::Right))
            }
            Some(Hit::Act(c, s)) => {
                self.settings.focus = Some((c, s));
                self.settings_key(KeyEvent::from(KeyCode::Enter))
            }
            None => false,
        }
    }

    /// Select and persist a theme — one mechanism with `/theme <name>`, down
    /// to what it says about when it takes effect.
    ///
    /// **This row selects; it does not switch.** The tree it came from held a
    /// process-wide active theme and repainted on the next frame. Here a theme
    /// is read once, at startup, into a `Copy` `Palette` that is already
    /// duplicated across the viewport, its view and `Term` — the ruling
    /// `session_command`'s `/theme` region argues in full, and the reason there
    /// is no `--save` on that command either. So the notice says the same thing
    /// `/theme` says: written, and in force from the next start.
    fn settings_theme(&mut self, name: String) {
        // The name must resolve before anything is written — `/theme`'s
        // ruling, and the reason the lookup is done before the write rather
        // than after it. The cycler only ever steps names `theme::names`
        // returned, so this is the belt to that braces.
        if !self.settings.themes.iter().any(|t| t == &name) {
            self.settings.notice = Some(format!("no theme named {name}"));
            return;
        }
        self.settings.theme = name.clone();
        self.settings.notice = Some(match self.home.as_deref() {
            Some(home) => match crate::session_command::write_theme(home, &name) {
                Ok(path) => format!(
                    "theme {name} — written to {}; in force from the next start",
                    path.display()
                ),
                Err(e) => format!("theme {name} — could not be written ({e})"),
            },
            None => format!(
                "theme {name} — no home directory, so there is nowhere to write the selection"
            ),
        });
    }

    /// Store the Enable Memory toggle, keeping the additive default-on
    /// semantics: On removes the key (absent means on), Off writes `false`.
    fn settings_memory(&mut self, on: bool) {
        let Some(home) = self.home.clone() else {
            self.settings.notice =
                Some("no home directory — settings.json cannot be written here".to_string());
            return;
        };
        let mut stored = crate::settings::load(&home);
        stored.memory = if on { None } else { Some(false) };
        match crate::settings::save(&home, &stored) {
            Ok(path) => {
                self.settings.memory_on = on;
                self.settings.notice = Some(if on {
                    format!(
                        "memory capture on — the default; the key is removed from {}",
                        path.display()
                    )
                } else {
                    format!("memory capture off — written to {}", path.display())
                });
            }
            Err(e) => self.settings.notice = Some(format!("settings could not be written: {e}")),
        }
    }

    /// Store the Prune History toggle, keeping the additive default-**off**
    /// semantics: Off removes the key (absent means off), On writes `true`.
    ///
    /// The mirror of [`Self::settings_memory`], and the asymmetry is the
    /// point: writing `false` here would leave a key in the file that says
    /// exactly what its absence says, and a settings file that grows a key
    /// every time somebody opens a screen is one nobody can diff.
    fn settings_prune(&mut self, on: bool) {
        let Some(home) = self.home.clone() else {
            self.settings.notice =
                Some("no home directory — settings.json cannot be written here".to_string());
            return;
        };
        let mut stored = crate::settings::load(&home);
        stored.prune_history = if on { Some(true) } else { None };
        match crate::settings::save(&home, &stored) {
            Ok(path) => {
                self.settings.prune_on = on;
                self.settings.notice = Some(if on {
                    format!(
                        "prune history on — written to {}; in force from the next run",
                        path.display()
                    )
                } else {
                    format!(
                        "prune history off — the default; the key is removed from {}",
                        path.display()
                    )
                });
            }
            Err(e) => self.settings.notice = Some(format!("settings could not be written: {e}")),
        }
    }

    /// `[ Save Now ]`: write settings.json as it stands, with a receipt. The
    /// live controls each persist on use, so this is a re-assertion — the
    /// receipt names the file so the choice is visible and removable.
    fn settings_save(&mut self) {
        let Some(home) = self.home.clone() else {
            self.settings.notice =
                Some("no home directory — settings.json cannot be written here".to_string());
            return;
        };
        let stored = crate::settings::load(&home);
        match crate::settings::save(&home, &stored) {
            Ok(path) => {
                self.settings.notice = Some(format!("settings written to {}", path.display()))
            }
            Err(e) => self.settings.notice = Some(format!("settings could not be written: {e}")),
        }
    }

    /// `[ Export ]`: a timestamped copy beside settings.json, receipt naming
    /// the path. The copy is byte-for-byte when the file exists; with no file
    /// yet, the current (empty) settings serialize instead of nothing.
    fn settings_export(&mut self) {
        let Some(home) = self.home.clone() else {
            self.settings.notice =
                Some("no home directory — settings.json cannot be written here".to_string());
            return;
        };
        let source = crate::settings::path(&home);
        let stamp = timestamp(std::time::SystemTime::now());
        let target = source.with_file_name(format!("settings-{stamp}.json"));
        let outcome = match std::fs::read(&source) {
            Ok(bytes) => std::fs::write(&target, bytes),
            Err(_) => serde_json::to_string_pretty(&crate::settings::load(&home))
                .map_err(std::io::Error::other)
                .and_then(|body| std::fs::write(&target, body)),
        };
        self.settings.notice = Some(match outcome {
            Ok(()) => format!("settings exported to {}", target.display()),
            Err(e) => format!("export failed: {e}"),
        });
    }

    /// The confirmed Reset: clear the additive keys this screen owns — theme
    /// and memory_capture — and say exactly that. Provider, models, tools and
    /// voice belong to other surfaces and are left alone.
    fn settings_reset(&mut self) {
        let Some(home) = self.home.clone() else {
            self.settings.notice =
                Some("no home directory — settings.json cannot be written here".to_string());
            return;
        };
        let mut stored = crate::settings::load(&home);
        stored.theme = None;
        stored.memory = None;
        // Added with the row: a key this screen can set is a key its Reset
        // has to clear, or Reset quietly means "most of it".
        stored.prune_history = None;
        match crate::settings::save(&home, &stored) {
            Ok(path) => {
                // Nothing to activate: the theme in force is the one this
                // process started with — see `settings_theme`. Clearing the key
                // is the whole of the reset, and the notice below already says
                // "cleared to defaults" rather than claiming a repaint.
                self.settings.theme = super::theme::BUILT_IN.to_string();
                self.settings.memory_on = true;
                self.settings.prune_on = false;
                self.settings.notice = Some(format!(
                    "reset: theme, memory capture and prune history cleared to defaults — \
                     written to {}",
                    path.display()
                ));
            }
            Err(e) => self.settings.notice = Some(format!("reset could not be written: {e}")),
        }
    }

    /// `[ Test ]`: a real reachability check where one is cheap — a bounded
    /// TCP connect to the local Ollama host. A hosted provider gets the
    /// honest notice instead: an unbounded DNS wait inside the input thread
    /// would freeze the page, and the next model call proves that path anyway.
    fn settings_test(&mut self) {
        use super::settings::TestState;
        if self.settings.provider != "ollama" {
            self.settings.notice = Some(format!(
                "{} is a hosted API — connectivity is proven by the next model call; \
                 Test pings a local Ollama host",
                self.settings.provider
            ));
            return;
        }
        let host = self
            .ollama_host_override
            .clone()
            .unwrap_or_else(ollama_host);
        let reached = ping(&host);
        self.settings.test = if reached {
            TestState::Ok
        } else {
            TestState::Fail
        };
        self.settings.notice = Some(if reached {
            format!("{host} answered")
        } else {
            format!("{host} did not answer")
        });
    }

    /// Point the settings mutations at a different home — a test's tempdir.
    #[cfg(test)]
    pub(crate) fn set_home(&mut self, home: std::path::PathBuf) {
        self.home = Some(home);
    }

    /// Aim the Test Connection ping at a listener a test owns.
    #[cfg(test)]
    pub(crate) fn set_ollama_host(&mut self, host: String) {
        self.ollama_host_override = Some(host);
    }

    /// Aim the harness feeds at a session directory a test owns.
    ///
    /// This file's own tests assign `harness_dir` directly, being inside the
    /// module that declares it. `term::guarantees` is a sibling and cannot, so
    /// anything it asserted about the Harness page would have been a claim
    /// about whatever `~/.emma/sessions` happened to hold on the machine
    /// running the suite — which is why that file recorded the page as
    /// undefended rather than write the assertion. This is the seam it named.
    #[cfg(test)]
    pub(crate) fn set_harness_dir(&mut self, dir: std::path::PathBuf) {
        self.harness_dir = dir;
    }

    /// Open or close the Memory page. Opening reads the project wiki fresh —
    /// creating it on first touch, which is when the built-in schema installs —
    /// so what the page shows is what is on disk right now.
    pub fn toggle_memory(&mut self, cwd: &str) {
        if self.memory.take().is_none() {
            self.memory = Some(memory_view_from(cwd));
            self.memory_cwd = cwd.to_string();
            self.settings_open = false;
            self.harness = None;
        }
    }

    pub fn memory_open(&self) -> bool {
        self.memory.is_some()
    }

    /// Alt+h. Reads real session history on every open — the dashboard shows
    /// what actually ran in this repo, not the mock's sample day.
    pub fn toggle_harness(&mut self, cwd: &str) {
        if self.harness.take().is_none() {
            self.harness = Some(harness_view_from(
                cwd,
                &self.harness_dir,
                self.home.as_deref(),
            ));
            self.harness_cwd = cwd.to_string();
            self.settings_open = false;
            self.memory = None;
        }
    }

    /// One key, while the Harness page is open. Returns whether the page
    /// consumed it; `false` lets the global layer (Alt+h, Ctrl-C…) keep the
    /// key. The shell half of the harness seam: the pure half is
    /// [`super::harness::handle_key`], and the two actions that need disk
    /// (refresh, inspect) come back here to touch it.
    pub fn harness_key(&mut self, key: ratatui::crossterm::event::KeyEvent) -> bool {
        use super::harness::{handle_key, HarnessAction};
        use ratatui::crossterm::event::{KeyEventKind, KeyModifiers};

        let Some(view) = self.harness.as_mut() else {
            return false;
        };
        if key.kind == KeyEventKind::Release
            || key
                .modifiers
                .intersects(KeyModifiers::ALT | KeyModifiers::CONTROL)
        {
            return false;
        }
        match handle_key(view, key) {
            HarnessAction::Refresh => self.refresh_harness(),
            HarnessAction::Inspect(id) => self.mount_inspect(&id),
            HarnessAction::Graph(id) => self.mount_graph(&id),
            HarnessAction::Close => {
                self.harness = None;
                self.harness_cwd.clear();
            }
            HarnessAction::None | HarnessAction::FocusChanged | HarnessAction::Help => {}
        }
        // Every plain key belongs to the open page, acted on or not — the
        // memory page's law, for the same reason: nothing else could
        // honestly reach it.
        true
    }

    /// A left-button press while the Harness page is open. Returns whether
    /// the page consumed it; `false` lets the press fall through (the
    /// sidebar's rule: a page where only one control works must not swallow
    /// the rest of the surface).
    pub fn harness_click(&mut self, col: u16, row: u16) -> bool {
        use super::harness::{hit, Hit, PageMode};
        if self.harness.is_none() {
            return false;
        }
        match hit(&self.harness_hits, col, row) {
            Some(Hit::Run(i)) => {
                if let Some(v) = self.harness.as_mut() {
                    if v.mode == PageMode::Dashboard {
                        v.selected_run = Some(i);
                        return true;
                    }
                }
                false
            }
            // A chip is its key: one dispatch path, so the two can never
            // disagree about what an action does.
            Some(Hit::Chip(c)) => self.harness_key(ratatui::crossterm::event::KeyEvent::from(
                ratatui::crossterm::event::KeyCode::Char(c),
            )),
            None => false,
        }
    }

    /// The wheel while the Harness page is open. Only the Run Graph's DAG
    /// canvas scrolls, and it is the whole page's one scrollable region, so
    /// the wheel is answered wherever it lands rather than only over the
    /// canvas rect. Returns whether the page consumed it; `false` lets the
    /// chat transcript underneath keep the wheel it has always had.
    pub fn harness_scroll(&mut self, up: bool) -> bool {
        use super::harness::PageMode;
        let Some(v) = self.harness.as_mut() else {
            return false;
        };
        if v.mode != PageMode::Graph {
            return false;
        }
        let Some(gv) = v.graph.as_mut() else {
            return false;
        };
        if gv.palette {
            return false;
        }
        super::rungraph::pan(gv, !up, WHEEL_GRAPH_ROWS);
        true
    }

    /// Mount the Inspect page for one run, from the stored log. A missing
    /// run becomes the page's notice, not a silent key.
    fn mount_inspect(&mut self, id: &str) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let dir = self.harness_dir.clone();
        let Some(view) = self.harness.as_mut() else {
            return;
        };
        match inspect_view_from(&dir, id, &view.version, now) {
            Some(iv) => {
                view.inspect = Some(iv);
                view.inspect_id = Some(id.to_string());
                view.mode = super::harness::PageMode::Inspect;
            }
            None => {
                view.notice = Some(format!(
                    "run {id} is not in the session logs; [R] refreshes"
                ));
            }
        }
    }

    /// Mount the Run Graph for one run, from the stored log. A missing run
    /// becomes the page's notice, not a silent key, which is `mount_inspect`'s rule.
    fn mount_graph(&mut self, id: &str) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let dir = self.harness_dir.clone();
        let Some(view) = self.harness.as_mut() else {
            return;
        };
        match rungraph_view_from(&dir, id, &view.version, now) {
            Some(gv) => {
                view.graph = Some(gv);
                view.graph_id = Some(id.to_string());
                view.mode = super::harness::PageMode::Graph;
            }
            None => {
                view.notice = Some(format!(
                    "run {id} is not in the session logs — [R] refreshes"
                ));
            }
        }
    }

    /// Re-read the session directory and carry the page's UI state (mode,
    /// selections, notice, the inspected run) onto the fresh data, clamped
    /// to it — `refresh_memory`'s law.
    fn refresh_harness(&mut self) {
        use super::harness::{PageMode, VISIBLE_RUNS};
        let Some(old) = self.harness.take() else {
            return;
        };
        let mut fresh =
            harness_view_from(&self.harness_cwd, &self.harness_dir, self.home.as_deref());
        fresh.mode = old.mode;
        fresh.notice = old.notice;
        fresh.graph = old.graph;
        fresh.graph_id = old.graph_id.clone();
        fresh.all_selected = old.all_selected.min(fresh.runs.len().saturating_sub(1));
        if !fresh.runs.is_empty() {
            let visible = fresh.runs.len().min(VISIBLE_RUNS);
            fresh.selected_run = Some(old.selected_run.unwrap_or(0).min(visible - 1));
        }
        let inspect_id = old.inspect_id.clone();
        self.harness = Some(fresh);
        // The mounted run is re-read too; gone from the logs, the page keeps
        // Inspect's own honest no-run state and says why.
        if let (PageMode::Inspect, Some(id)) = (old.mode, inspect_id) {
            self.mount_inspect(&id);
        }
        if let (PageMode::Graph, Some(id)) = (old.mode, old.graph_id) {
            self.mount_graph(&id);
        }
    }

    pub fn harness_open(&self) -> bool {
        self.harness.is_some()
    }

    /// One key, while the Memory page is open. Returns whether the page
    /// consumed it; `false` lets the global layer (Alt+m, Ctrl-C, PgUp…)
    /// keep the key. This is the shell half of the M5 seam: the pure half is
    /// [`super::memory::handle_key`], and every mutation it asks for goes
    /// through [`crate::memory::Wiki`] and then rebuilds the view, so the
    /// page always shows disk truth.
    pub fn memory_key(&mut self, key: ratatui::crossterm::event::KeyEvent) -> bool {
        use super::memory::{handle_key, MemoryAction, NOTICE_M2};
        use ratatui::crossterm::event::{KeyEventKind, KeyModifiers};

        let Some(view) = self.memory.as_mut() else {
            return false;
        };
        if key.kind == KeyEventKind::Release
            || key
                .modifiers
                .intersects(KeyModifiers::ALT | KeyModifiers::CONTROL)
        {
            return false;
        }
        match handle_key(view, key) {
            MemoryAction::Refresh => self.refresh_memory(),
            MemoryAction::Pin(slug) => self.memory_mutate(|w| w.pin(&slug).map(drop), None),
            MemoryAction::Unpin(slug) => self.memory_mutate(|w| w.unpin(&slug).map(drop), None),
            MemoryAction::Archive(slug) => self.memory_mutate(|w| w.archive(&slug), None),
            MemoryAction::Reindex => self.memory_mutate(
                |w| w.rebuild_index().map(drop),
                Some("Index rebuilt from the pages on disk".to_string()),
            ),
            MemoryAction::Add { title, category } => {
                let cat = crate::memory::Category::ALL[category.min(5)];
                let ok = format!("Added \"{title}\" to {cat}");
                // The typed text is title and body both — the M5 add flow is
                // one line; the model's ingest pass (M3) writes real bodies.
                self.memory_mutate(
                    |w| w.create(&title, cat, "memory-page", &title).map(drop),
                    Some(ok),
                );
            }
            // Answering needs retrieval (M2). Saying so beats silence.
            MemoryAction::Query(_) => {
                if let Some(v) = self.memory.as_mut() {
                    v.notice = Some(NOTICE_M2.to_string());
                }
            }
            MemoryAction::None | MemoryAction::FocusChanged | MemoryAction::Help => {}
        }
        // Every plain key belongs to the open page, acted on or not: with
        // no chat input underneath there is nothing else it could honestly
        // reach. Chords and releases returned early above.
        true
    }

    /// Run one store mutation, then rebuild the view from disk. An error
    /// becomes the page's notice — a rig problem must never look like state.
    fn memory_mutate(
        &mut self,
        op: impl FnOnce(&crate::memory::Wiki) -> anyhow::Result<()>,
        ok_notice: Option<String>,
    ) {
        let outcome = crate::memory::Wiki::project(std::path::Path::new(&self.memory_cwd))
            .and_then(|w| op(&w));
        self.refresh_memory();
        if let Some(v) = self.memory.as_mut() {
            match outcome {
                Ok(()) => {
                    if ok_notice.is_some() {
                        v.notice = ok_notice;
                    }
                }
                Err(e) => v.notice = Some(format!("memory error: {e}")),
            }
        }
    }

    /// Re-read the wiki and carry the page's UI state (mode, focus, filter,
    /// selections, half-typed query) onto the fresh data, clamped to it.
    fn refresh_memory(&mut self) {
        use super::memory::Focus;
        let Some(old) = self.memory.take() else {
            return;
        };
        let mut fresh = memory_view_from(&self.memory_cwd);
        fresh.mode = old.mode;
        fresh.query = old.query;
        fresh.adding = old.adding;
        fresh.notice = old.notice;
        fresh.all_filter = old.all_filter;
        fresh.all_selected = old.all_selected.min(fresh.all.len().saturating_sub(1));
        fresh.pin_selected = old.pin_selected.min(fresh.pinned.len().saturating_sub(1));
        fresh.focus = match old.focus {
            Some(Focus::RecentRow(_)) if fresh.recent.is_empty() => Some(Focus::RecentViewAll),
            Some(Focus::RecentRow(i)) => Some(Focus::RecentRow(i.min(fresh.recent.len() - 1))),
            Some(Focus::PinnedDots(_)) if fresh.pinned.is_empty() => Some(Focus::PinnedManage),
            Some(Focus::PinnedDots(i)) => Some(Focus::PinnedDots(i.min(fresh.pinned.len() - 1))),
            f => f,
        };
        self.memory = Some(fresh);
    }

    pub fn settings_open(&self) -> bool {
        self.settings_open
    }

    /// The user toggled the sidebar. Latches in both directions — see [`Latch`].
    pub fn toggle_sidebar(&mut self, total_cols: u16) {
        self.latch = Latch {
            collapsed: !hidden(total_cols, self.latch),
            by_user: true,
        };
    }

    /// The run's identity arrived: SESSIONS fills with the sessions that ran
    /// in this directory, and the transcript learns where the complete record
    /// lives so the cap marker can point at it.
    pub fn set_identity(&mut self, session: &str, transcript_path: &str, cwd: &str, skin: &Skin) {
        if !session.is_empty() {
            // The session directory is the transcript's own parent rather than
            // `SessionLog::default_dir`'s answer: this run is writing into it,
            // so it is the directory that is certainly right, `EMMA_SESSION_DIR`
            // and `--session-dir` included, with no second lookup to disagree.
            self.scope = Some(SessionScope {
                dir: std::path::Path::new(transcript_path)
                    .parent()
                    .map(std::path::Path::to_path_buf),
                cwd: cwd.to_string(),
                current: session.to_string(),
            });
            self.refresh_sessions();
        }
        if !transcript_path.is_empty() {
            self.transcript.record_at(transcript_path, skin);
        }
    }

    /// Rebuild SESSIONS from the session directory.
    ///
    /// **Read on demand, never polled.** The list changes when a goal ends —
    /// this run's own name is its latest goal, and a sibling `emma` in another
    /// terminal has finished one by then too — so [`super::frame::Frame`] calls
    /// this from `goal_ended` and nowhere else. That is once per goal against a
    /// directory of small files, which is cheaper than any interval that would
    /// be quick enough to feel live.
    pub fn refresh_sessions(&mut self) {
        let Some(scope) = &self.scope else { return };
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let offset = harness_state::local_offset_secs();

        // The current run leads the list whatever the directory says. At
        // `set_identity` its file is empty — the first goal has not been typed
        // — so the cwd filter cannot see it, and the row the reader is sitting
        // on is the one row that must never be missing.
        let mut rows = vec![sidebar::Row {
            name: current_name(&scope.current),
            trailing: relative_time(now_ms, now_ms, offset),
            selected: true,
        }];
        let feed = scope
            .dir
            .as_deref()
            .map(|dir| harness_state::sessions_for(dir, std::path::Path::new(&scope.cwd), now_ms))
            .transpose()
            .ok()
            .flatten()
            .unwrap_or_default();
        for s in feed.sessions {
            if rows.len() >= harness_state::SIDEBAR_SESSIONS {
                break;
            }
            if s.id == scope.current {
                // The directory knows this run's latest goal and the seed row
                // does not, so the real name replaces the id tail in place.
                rows[0].name = s.name;
                rows[0].trailing = relative_time(s.last_event_ms, now_ms, offset);
                continue;
            }
            rows.push(sidebar::Row {
                name: s.name,
                trailing: relative_time(s.last_event_ms, now_ms, offset),
                selected: false,
            });
        }
        self.side.sessions = rows;
    }

    /// Whether a left-button press landed on the SESSIONS header's `[+]`.
    ///
    /// `prompt_pending` is the shell's to know and is passed in rather than
    /// looked up, which is what keeps the decision a pure function.
    pub fn new_session_clicked(&self, col: u16, row: u16, prompt_pending: bool) -> bool {
        !prompt_pending && self.sessions_add.is_some_and(|rect| within(rect, col, row))
    }

    /// The TOOLS catalogue, as rows — mapped by [`tool_rows`] and handed in so
    /// this stays testable without the catalogue's filesystem probing.
    ///
    /// **What it is for after the import is narrower than what it was for
    /// before it.** The replaced shell used these rows *as* the TOOLS section.
    /// The mock's section is now `sidebar::tool_rows`, which knows the glyphs,
    /// the chords and which screen is open but nothing about what is installed
    /// on the box. So these rows are kept for the one fact only they carry:
    /// [`sidebar::TOOL_MISSING`] on a tool whose probe found nothing to run.
    /// A chord rendered beside a program that is not there is the defect
    /// DEF-037 was filed for.
    pub fn set_tools(&mut self, rows: Vec<sidebar::Row>) {
        self.tools = rows;
    }

    /// Draw the whole window. Returns where the cursor belongs, exactly as
    /// [`View::render`] does for the inline viewport.
    ///
    /// Every screen goes through [`super::layout::compose`]: the sidebar, the
    /// main border and the bottom bar are the seam's, and only the main
    /// region's occupant varies here.
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

        // The TOOLS and QUICK HELP sections are the sidebar's canonical
        // rows on every screen; the current screen only decides which tool
        // row is selected. A temporary state, not a mutation: switching
        // screens must cost nothing.
        let ascii = skin.glyphs == super::render::ASCII;
        let side = sidebar::State {
            sessions: self.side.sessions.clone(),
            commands: unavailable_marked(
                sidebar::tool_rows(
                    ascii,
                    if self.settings_open {
                        Some(sidebar::Tool::Settings)
                    } else if self.memory.is_some() {
                        Some(sidebar::Tool::Memory)
                    } else if self.harness.is_some() {
                        Some(sidebar::Tool::Harness)
                    } else {
                        None
                    },
                ),
                &self.tools,
            ),
            help: sidebar::quick_help(ascii),
            today: Some(today_local()),
            collapsed,
        };

        // ⚠ THE PANE IS THE ONLY SURFACE THAT SELECTS, so a screen that is not
        // chat has no selectable area at all: the rectangle a click is tested
        // against is emptied here rather than left over from the last chat
        // paint, and anything highlighted goes with it. Settings and Memory
        // are out of scope by the owner's decision, and "out of scope" has to
        // mean the mouse cannot reach them, not that nobody has tried.
        if self.settings_open || self.memory.is_some() {
            self.chat_rect = Rect::new(0, 0, 0, 0);
            self.scrollbar = None;
            self.bar_grab = None;
            self.notice = None;
            self.selection = None;
            self.chat_cells.clear();
        }
        // Stale control rects must not keep catching clicks after the page
        // closes or a sub-page takes over; the pages' paints refill them.
        self.harness_hits = super::harness::Hits::default();
        self.settings_hits = super::settings::Hits::default();

        // The main region's width is what the input box wraps to, so the
        // sidebar has to be measured first — see `dock_height`'s `width`.
        let dock_h = dock_height(
            view,
            area.height,
            Some(
                area.width
                    .saturating_sub(sidebar::width(area.width, collapsed)),
            ),
        );
        super::layout::compose(area, buf, view, bar, &side, dock_h, |r, buf| {
            // A collapsed sidebar has a zero-width region, so this clears
            // itself: there is no control on screen and no cell that acts
            // like one.
            self.sessions_add = new_session_hit(r.sidebar);
            if self.settings_open {
                self.settings_screen(r, buf, view);
                // The cursor is parked because there is nothing to type into.
                None
            } else if let Some(mv) = &self.memory {
                super::memory::render(r.main, buf, mv, &view.skin);
                None
            } else if let Some(hv) = &self.harness {
                // The paint reports where the clickable controls landed;
                // `harness_click` hit-tests against exactly that (the
                // sessions-[+] rule, with the rects straight off the paint).
                let hits = super::harness::render_hits(r.main, buf, hv, &view.skin);
                // The DAG canvas reports its own height, which is the only
                // part of the viewport a pure key handler cannot know. Store
                // it on the view and clamp what a smaller terminal has just
                // made unreachable, so the next key starts from the truth.
                let rows = hits.graph_canvas_rows;
                self.harness_hits = hits;
                if let Some(gv) = self.harness.as_mut().and_then(|h| h.graph.as_mut()) {
                    gv.canvas_rows = rows;
                    gv.scroll = gv.scroll.min(super::rungraph::max_scroll(gv));
                }
                None
            } else {
                self.chat_screen(r, buf, view)
            }
        })
    }

    /// The Settings screen owns everything between the main border and the
    /// status bar: header, rule, chat, dock and hint rows together, which is
    /// the mock's main panel — it shows no input box.
    fn settings_screen(&mut self, r: &Regions, buf: &mut Buffer, view: &View) {
        let bottom = r.hint.y + r.hint.height;
        let pane = Rect::new(
            r.header.x,
            r.header.y,
            r.header.width,
            bottom.saturating_sub(r.header.y),
        );
        // The `View`-owned rows are read at paint time so they can never go
        // stale; the interaction state (focus, notices, the disk-backed rows)
        // lives on across paints.
        self.settings.version = concat!("v", env!("CARGO_PKG_VERSION")).to_string();
        self.settings.model = view.status.model.clone();
        self.settings.cwd = view.status.cwd.clone();
        // The two meters, from the same `Status` the status line reads, at the
        // same moment. Paint-time rather than open-time on purpose: these move
        // while the screen is up, and a context figure frozen at the toggle is
        // the stale readout `render.rs`'s status region exists to not have.
        self.settings.context = view.status.context;
        self.settings.goal_budget = view.status.spend.map(|(_, cap)| cap);
        // The paint reports where the controls landed; `settings_click`
        // hit-tests against exactly that (the harness's rule).
        self.settings_hits = settings::render_hits(pane, buf, &self.settings, &view.skin);
    }

    /// The chat screen: header, rule, transcript, dock and hint, inside the
    /// chrome the seam already painted.
    fn chat_screen(&mut self, r: &Regions, buf: &mut Buffer, view: &View) -> Option<Position> {
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
        self.chat_height = r.chat.height.max(1);
        self.wrap_width = chat::message_width(r.chat.width.max(1));
        self.transcript.set_width(skin, self.wrap_width);

        // The bar is an affordance for a state — more transcript than pane —
        // so the question is asked at the width the pane actually has, and
        // only then is the column taken out of it.
        //
        // ⚠ THE RE-WRAP CANNOT OSCILLATE, which is the reason this is a
        // reserved column rather than an overlay. Narrowing the message
        // column can only ever produce *more* rows, so a transcript that
        // overflowed at the full width still overflows at one column less:
        // the bar appears once and stays. An overlay would need no second
        // wrap at all, and would paint over the last cell of every row that
        // reached the column — which is the tail of a wrapped sentence.
        let bar = u16::from(
            r.chat.width >= SCROLLBAR_MIN_COLS
                && self.transcript.total_rows() > usize::from(r.chat.height),
        );
        let pane = Rect {
            width: r.chat.width - bar,
            ..r.chat
        };
        if bar > 0 {
            self.wrap_width = chat::message_width(pane.width.max(1));
            self.transcript.set_width(skin, self.wrap_width);
        }
        chat::render(pane, buf, &self.transcript, skin, self.notice.as_deref());
        self.chat_rect = pane;
        self.scrollbar = (bar > 0).then(|| pane.right());
        if let Some(x) = self.scrollbar {
            scrollbar(
                Rect::new(x, pane.y, 1, pane.height),
                buf,
                &self.transcript,
                skin,
            );
        }
        self.highlight(pane, buf);
        self.chat_cells = snapshot(pane, buf);

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
        cursor
    }

    // -----------------------------------------------------------------------
    // The scrollbar as a control
    //
    // ⚠ THE BAR ANSWERS FIRST, AND IT CANNOT COLLIDE. Its column was taken
    // out of `chat_rect` before it was painted, so "a press on the bar" and
    // "a press in the pane" are disjoint by arithmetic rather than by an
    // ordering somebody has to keep — which matters because the selection is
    // the other thing a left-button press can mean, and the two would
    // otherwise both fire on the one event.
    //
    // The step sizes are borrowed, not invented: the track pages by
    // [`Self::page`], the same page PgUp gives, and the drag lands an offset
    // through [`transcript::drag_offset`], the inverse of the geometry the
    // thumb was drawn from. Nothing here holds a scroll position of its own.
    // -----------------------------------------------------------------------

    /// The button went down. `true` when it landed on the bar and the bar did
    /// something about it — the caller then does *not* start a selection.
    pub fn bar_press(&mut self, col: u16, row: u16) -> bool {
        let Some(x) = self.scrollbar else {
            return false;
        };
        let pane = self.chat_rect;
        if col != x || row < pane.y || row >= pane.bottom() {
            return false;
        }
        // Dead travel first: an offset past the last useful row draws the
        // same screen as the last useful row, and a drag measured from it
        // would spend its first pixels moving nothing.
        let top = transcript::max_offset(self.transcript.total_rows(), pane.height);
        if self.transcript.scroll_offset() > top {
            self.transcript.scroll_to(top);
        }
        let hit = transcript::grab(
            self.transcript.total_rows(),
            pane.height,
            self.transcript.scroll_offset(),
            row - pane.y,
        );
        match hit {
            Some(transcript::Grab::Thumb(_)) => {
                self.bar_grab = Some((self.transcript.scroll_offset(), row - pane.y));
            }
            Some(transcript::Grab::PageUp) => {
                self.transcript.scroll_up(self.page());
                if self.transcript.scroll_offset() > top {
                    self.transcript.scroll_to(top);
                }
            }
            Some(transcript::Grab::PageDown) => self.transcript.scroll_down(self.page()),
            None => return false,
        }
        // A press on the bar dismisses a highlight, the way a press on any
        // other piece of chrome does. Reaching for the scrollbar is not a
        // gesture anybody makes while still wanting the old selection.
        self.selection = None;
        self.notice = None;
        true
    }

    /// The pointer moved with the thumb held. `true` when the view moved,
    /// which is the answer the caller repaints on.
    pub fn bar_drag(&mut self, row: u16) -> bool {
        let Some(press) = self.bar_grab else {
            return false;
        };
        let pane = self.chat_rect;
        if pane.height == 0 {
            return false;
        }
        // Clamped rather than dropped: a pointer dragged off the pane is an
        // ordinary drag, and the two clamps are the two ends of the track.
        let track_row = row.clamp(pane.y, pane.bottom() - 1) - pane.y;
        let offset =
            transcript::drag_offset(self.transcript.total_rows(), pane.height, press, track_row);
        let moved = offset != self.transcript.scroll_offset();
        self.transcript.scroll_to(offset);
        moved
    }

    /// The button came up. `true` when a drag was live — a release with no
    /// drag behind it belongs to whatever else was going on.
    pub fn bar_release(&mut self) -> bool {
        self.bar_grab.take().is_some()
    }

    /// Whether the thumb is being dragged right now.
    pub fn bar_dragging(&self) -> bool {
        self.bar_grab.is_some()
    }

    // -----------------------------------------------------------------------
    // Selecting text in the chat pane
    //
    // ⚠ THIS EXISTS ONLY WHILE MOUSE CAPTURE IS ON, and that is not a rule
    // written here — it is what the terminal does. Capture is what routes a
    // drag to this program at all; Ctrl-S hands the mouse back, no mouse
    // event arrives, and the terminal's own selection works exactly as it
    // does in every other program. So the two selections cannot both be
    // live, no mode flag decides between them, and the documented fallback
    // stays the fallback.
    //
    // The pane is the only surface that selects. The sidebar, the input box
    // and the bar are chrome, and a selection over them would copy furniture.
    // -----------------------------------------------------------------------

    /// The button went down. `true` when it landed in the chat pane and a
    /// selection started; `false` clears whatever was selected, because a
    /// click elsewhere is the ordinary way a person dismisses one.
    pub fn selection_begin(&mut self, col: u16, row: u16) -> bool {
        self.notice = None;
        match self.cell_at(col, row) {
            Some(cell) => {
                self.selection = Some((cell, cell));
                true
            }
            None => {
                self.selection = None;
                false
            }
        }
    }

    /// The pointer moved with the button down. Clamped into the pane rather
    /// than dropped: a drag that runs off the edge is a normal drag, and
    /// losing it there would leave a selection that stops mid-word.
    pub fn selection_extend(&mut self, col: u16, row: u16) {
        let Some((anchor, _)) = self.selection else {
            return;
        };
        let pane = self.chat_rect;
        if pane.width == 0 || pane.height == 0 {
            return;
        }
        let x = col.clamp(pane.x, pane.right() - 1) - pane.x;
        let y = row.clamp(pane.y, pane.bottom() - 1) - pane.y;
        self.selection = Some((anchor, (y, x)));
    }

    /// What a mouse selection just sent to the clipboard, for the indicator
    /// slot. The wording and the token estimate are
    /// [`chat::sent_notice`]'s — one place decides what this claims.
    pub fn notice_sent(&mut self, chars: usize) {
        self.notice = Some(chat::sent_notice(chars));
    }

    /// Drop the notice. `true` when there was one, which is the answer the
    /// caller repaints on.
    pub fn notice_clear(&mut self) -> bool {
        self.notice.take().is_some()
    }

    /// Whether a drag is live.
    pub fn has_selection(&self) -> bool {
        self.selection.is_some()
    }

    /// Drop the selection. `true` when there was one to drop — the caller
    /// repaints on that answer and not otherwise.
    pub fn selection_clear(&mut self) -> bool {
        self.selection.take().is_some()
    }

    /// The selected text, as the reader sees it: rendered rows, wrapping
    /// included, styling gone. Empty when nothing is selected.
    pub fn selection_text(&self) -> String {
        match self.selection {
            Some((a, b)) => selected_text(&self.chat_cells, a, b),
            None => String::new(),
        }
    }

    /// A buffer cell inside the chat pane, in pane coordinates.
    fn cell_at(&self, col: u16, row: u16) -> Option<Cell> {
        let pane = self.chat_rect;
        pane.contains(Position::new(col, row))
            .then(|| (row - pane.y, col - pane.x))
    }

    /// Reverse the cells under the selection.
    ///
    /// Reverse video rather than an accent band: it is the one emphasis every
    /// palette level has, it inverts whatever the row was already wearing —
    /// prose, a diff, a code fence — and it cannot collide with a colour the
    /// transcript is using to mean something else.
    fn highlight(&self, pane: Rect, buf: &mut Buffer) {
        let Some((a, b)) = self.selection else {
            return;
        };
        let (first, last) = order(a, b);
        for y in first.0..=last.0 {
            if y >= pane.height {
                break;
            }
            let from = if y == first.0 { first.1 } else { 0 };
            let to = if y == last.0 { last.1 } else { pane.width - 1 };
            for x in from..=to.min(pane.width - 1) {
                buf[(pane.x + x, pane.y + y)].modifier |= Modifier::REVERSED;
            }
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

// region: The selection
// ---------------------------------------------------------------------------
// The selection
//
// Cells in, text out. Pure, so the one question worth asking — what does a
// drag from here to there put on the clipboard — is answered by a test and
// not by a terminal, a mouse and a person watching.
// ---------------------------------------------------------------------------

/// A cell of the chat pane: `(row, column)`, pane-relative.
pub type Cell = (u16, u16);

/// The anchor and head in reading order.
fn order(a: Cell, b: Cell) -> (Cell, Cell) {
    if (a.0, a.1) <= (b.0, b.1) {
        (a, b)
    } else {
        (b, a)
    }
}

/// The text of a selection over painted cells.
///
/// **Reading order, not a rectangle.** The first row runs to its end, the
/// last stops at the pointer, and the rows between are whole — which is what
/// a person dragging across a wrapped paragraph is pointing at. A block
/// selection would cut the paragraph into a column of fragments, and the
/// wrapping the pane did is exactly what makes the columns meaningless.
///
/// The head cell is included: a drag that ends on a character has selected
/// it. Trailing padding goes, because the blank right of a short row is the
/// pane's, not the reader's; interior blank rows stay, because a separator
/// the reader dragged across is a paragraph break they can see.
pub fn selected_text(cells: &[Vec<String>], a: Cell, b: Cell) -> String {
    let (first, last) = order(a, b);
    let mut out: Vec<String> = Vec::new();
    for y in first.0..=last.0 {
        let Some(row) = cells.get(usize::from(y)) else {
            break;
        };
        let from = usize::from(if y == first.0 { first.1 } else { 0 });
        let to = if y == last.0 {
            usize::from(last.1).saturating_add(1).min(row.len())
        } else {
            row.len()
        };
        let text: String = row.get(from..to).unwrap_or_default().concat();
        out.push(text.trim_end().to_string());
    }
    // A selection that is all padding is nothing at all, not a run of empty
    // lines nobody asked to copy.
    while out.last().is_some_and(|l| l.is_empty()) {
        out.pop();
    }
    out.join("\n")
}

/// The pane's cells, one symbol each, as they were just painted.
fn snapshot(pane: Rect, buf: &Buffer) -> Vec<Vec<String>> {
    let pane = pane.intersection(buf.area);
    (pane.y..pane.bottom())
        .map(|y| {
            (pane.x..pane.right())
                .map(|x| buf[(x, y)].symbol().to_string())
                .collect()
        })
        .collect()
}

// endregion: The selection

// region: The scrollbar
// ---------------------------------------------------------------------------
// The scrollbar
//
// One column at the right edge of the chat pane. The geometry is
// [`transcript::thumb`] — the transcript's own scroll offset read a second
// way — so there is exactly one scroll position in the program and the bar
// cannot drift from the keys.
//
// Drawn here rather than with `ratatui`'s `Scrollbar` widget: that widget
// wants a `ScrollbarState` of its own, counted from the top, and this
// transcript counts from the tail because appending has to leave a scrolled
// view where it is. Translating between the two every frame is a second
// position to keep in step, which is the defect this whole arrangement
// exists to avoid — and it would still not give the ASCII fallback the glyph
// set says this program needs.
// ---------------------------------------------------------------------------

/// Below this many columns the pane keeps every cell for the message. The
/// scrolled-behind indicator still says where the reader is, in words.
const SCROLLBAR_MIN_COLS: u16 = 24;

/// Rows one wheel notch moves the DAG canvas. Three, the transcript's own
/// `WHEEL_ROWS`, so the two surfaces feel the same under the same hand.
const WHEEL_GRAPH_ROWS: u16 = 3;

/// Paint the bar into its one-column area. Nothing when it all fits.
fn scrollbar(area: Rect, buf: &mut Buffer, t: &Transcript, skin: &Skin) {
    let area = area.intersection(buf.area);
    if area.width == 0 || area.height == 0 {
        return;
    }
    let Some((top, len)) = transcript::thumb(t.total_rows(), area.height, t.scroll_offset()) else {
        return;
    };
    // `Glyphs` has no bar yet and growing it belongs to the file that owns the
    // glyph sets — the same seam `chat::indicator` names. The consts are
    // `PartialEq` for exactly this question.
    let (track, thumb) = if skin.glyphs == super::render::ASCII {
        ("|", "#")
    } else {
        ("\u{2502}", "\u{2588}")
    };
    for i in 0..area.height {
        let on_thumb = i >= top && i < top + len;
        let (glyph, style) = if on_thumb {
            (thumb, skin.palette.style(Role::Accent))
        } else {
            (track, skin.palette.dim())
        };
        buf.set_stringn(area.x, area.y + i, glyph, 1, style);
    }
}

// endregion: The scrollbar

/// The TOOLS section as `sidebar::Row`s, from the user-tool catalogue.
///
/// The trailing column is the **real binding** — `Alt+<key>`, or the bare key
/// where the chord is one — so the legend cannot advertise a chord the decoder
/// does not answer. [`sidebar::TOOL_MISSING`] where the probe found nothing to
/// run: a row that named a program that is not installed is a key that does
/// nothing, and the sidebar dims the whole row on that exact trailing.
pub fn tool_rows(entries: &[crate::usertools::Entry]) -> Vec<sidebar::Row> {
    entries
        .iter()
        .map(|e| sidebar::Row {
            name: e.label.clone(),
            trailing: if !e.available {
                sidebar::TOOL_MISSING.to_string()
            } else if e.key == '/' {
                "/".to_string()
            } else {
                format!("Alt+{}", e.key)
            },
            selected: false,
        })
        .collect()
}

/// Put `n/a` on the mock's rows whose tool the catalogue could not find.
///
/// **The seam between the two TOOLS tables, and the only place they meet.**
/// `sidebar::tool_rows` knows the glyphs, the names, the chords and which
/// screen is open; [`tool_rows`] knows what is installed. Matched on the label,
/// because that is the one field both mint from the same words — and a label
/// the catalogue does not carry is simply left alone rather than guessed at.
/// An empty catalogue changes nothing, which is what an inline run wants.
fn unavailable_marked(
    mut rows: Vec<sidebar::Row>,
    catalogue: &[sidebar::Row],
) -> Vec<sidebar::Row> {
    for row in &mut rows {
        let missing = catalogue
            .iter()
            .find(|c| row.name.ends_with(&c.name))
            .is_some_and(|c| c.trailing == sidebar::TOOL_MISSING);
        if missing {
            row.trailing = sidebar::TOOL_MISSING.to_string();
        }
    }
    rows
}

/// The QUICK HELP table: the real keymap, nothing aspirational.
///
/// **The rows are derived, not typed.** A `(key, description)` pair written by
/// hand carries no reference to the `match` arm that answers it, so the two
/// drift and nothing says so — the fork inventory counted
/// a branch's copy of this panel advertising six keys of which four do nothing,
/// one of them `Ctrl+k` for a binding that is `Ctrl-U`. The rows come from
/// [`super::bindings::CHAT`], where each carries the chord it means, and the
/// tests there drive every one through the real decoders. Both paint sites —
/// the sidebar's panel and the Settings page's `Keys` block — read this one
/// value, so there is still exactly one table on screen.
pub(crate) fn keymap() -> Vec<(String, String)> {
    super::bindings::CHAT.hints()
}

/// The last path component, for the status bar's ENV cell. Local rather than
/// borrowed from `render.rs`, whose copy is private and whose signature may
/// move with the cell renderers.
/// The Harness dashboard's data: real session history, read on the spot.
///
/// The seam mirrors `memory_view_from`: harness_state enumerates what the
/// session logs can honestly back, and this function is the entire coupling.
/// Workers, task queue, and resources render their empty states because no
/// backing record exists (plan HB4); runs, events, gates, and the runtime trio
/// are real.
fn harness_view_from(
    cwd: &str,
    dir: &std::path::Path,
    home: Option<&std::path::Path>,
) -> super::harness::HarnessView {
    use super::harness::{HarnessView, Run, RunState, RuntimeView};
    use crate::harness_state as hs;

    let mut v = HarnessView {
        version: concat!("v", env!("CARGO_PKG_VERSION")).to_string(),
        runtime: RuntimeView {
            binary: "emma".to_string(),
            version: concat!("v", env!("CARGO_PKG_VERSION")).to_string(),
            workspace: cwd.to_string(),
            mode: "local".to_string(),
            ..RuntimeView::default()
        },
        ..HarnessView::default()
    };
    if let Ok(g) = hs::gates(crate::approval::current_gate(), None) {
        v.auto_approve = g.mode_label.to_string();
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let Ok(feed) = hs::runs(dir, now) else {
        // The memory page's rule from this side: an unreadable session
        // directory and one with no runs in it render the same dashboard, and
        // they mean opposite things. Say which store failed.
        v.notice = Some(super::harness::NOTICE_UNREADABLE.to_string());
        return v;
    };
    let offset = hs::local_offset_secs();
    let mine: Vec<&hs::RunRow> = feed
        .runs
        .iter()
        .filter(|r| r.cwd.as_deref() == Some(cwd))
        .collect();
    // Every run, newest first: the dashboard shows three, ALL RUNS the rest.
    v.runs = mine
        .iter()
        .map(|r| Run {
            name: r.name.clone(),
            key: r.id.clone(),
            state: match r.status {
                hs::RunStatus::Running => RunState::Running,
                hs::RunStatus::Completed => RunState::Completed,
                hs::RunStatus::Failed => RunState::Failed,
                // Stalled is not paused-by-a-person, but "quiet, unfinished"
                // is closer to Paused than to anything else the page has.
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
            stamp: clock(r.started_ms, offset),
            done: r.progress.as_ref().map(|p| p.done as u32).unwrap_or(0),
            total: r.progress.as_ref().map(|p| p.total as u32).unwrap_or(0),
        })
        .collect();
    if !v.runs.is_empty() {
        v.selected_run = Some(0);
    }
    // The EVENT LOG eats from the newest session here — the tail the mock's
    // card is (plan HB2, the session-log tail, leveled) — and a running run
    // is the trace header's Current Run claim.
    if let Some(newest) = mine.first() {
        let path = dir.join(format!("{}.jsonl", newest.session));
        if let Ok(feed) = hs::events(&path, DASHBOARD_EVENTS) {
            v.events = feed
                .lines
                .iter()
                .map(|e| super::harness::Event {
                    time: clock(e.at_ms, offset),
                    level: match e.level {
                        hs::Level::Info => super::harness::LogLevel::Info,
                        hs::Level::Warn => super::harness::LogLevel::Warn,
                        hs::Level::Error => super::harness::LogLevel::Error,
                    },
                    text: e.line.clone(),
                })
                .collect();
        }
    }
    if let Some(run) = v.runs.iter().find(|r| r.state == RunState::Running) {
        v.current_run = run.id.clone();
    }
    // RUNTIME STATUS's live rows. Every one of them is read off the newest
    // run in this repo rather than derived a second time, so the card and the
    // ACTIVE RUNS row above it can never disagree about what is happening.
    // With no run on disk they stay empty and the card draws its dashes.
    v.runtime.runtime = ASYNC_RUNTIME.to_string();
    let settings = home.map(crate::settings::load).unwrap_or_default();
    v.runtime.provider = settings
        .provider
        .clone()
        .unwrap_or_else(|| emma_llm::DEFAULT_PROVIDER.to_string());
    if let Some(newest) = mine.first() {
        v.runtime.model = newest
            .model
            .clone()
            .or_else(|| settings.models.get(&v.runtime.provider).cloned())
            .unwrap_or_default();
        v.runtime.started = clock(newest.started_ms, offset);
        v.runtime.uptime = fmt_duration(run_span_ms(newest, now));
        // "Healthy" is the newest run's own verdict: a run that is going or
        // that finished cleanly is the only thing this page can call well.
        v.runtime.health = Some(matches!(
            newest.status,
            hs::RunStatus::Running | hs::RunStatus::Completed
        ));
    }
    v
}

/// The async runtime this binary is built on. A fact about the build, so it
/// is a constant here rather than a value somebody has to keep true.
const ASYNC_RUNTIME: &str = "tokio";

/// How long a run has been going: its own reported elapsed when it finished,
/// the clock while it is live, and its record span when it went quiet without
/// an ending. Never an estimate.
fn run_span_ms(row: &crate::harness_state::RunRow, now_ms: u64) -> u64 {
    row.elapsed_ms.unwrap_or(match row.status {
        crate::harness_state::RunStatus::Running => now_ms.saturating_sub(row.started_ms),
        _ => row.last_event_ms.saturating_sub(row.started_ms),
    })
}

/// How many event lines the dashboard's card asks for. Eight is the mock's
/// visual budget; the card compresses further on its own when short.
const DASHBOARD_EVENTS: usize = 8;

/// The Inspect page's data for one run, out of the stored session logs.
///
/// **Honest mapping** (harness-live): the tool-call list and the event
/// stream are the run's real content; steps, artifacts, ETA, context and
/// branch have no backing record and stay their honest empties. `None`
/// when no session file holds the id.
fn inspect_view_from(
    dir: &std::path::Path,
    id: &str,
    version: &str,
    now_ms: u64,
) -> Option<super::inspect::InspectView> {
    use super::inspect as ins;
    use crate::harness_state as hs;

    let d = hs::detail(dir, id, now_ms).ok().flatten()?;
    let offset = hs::local_offset_secs();
    let row = &d.row;
    let (status, health) = match row.status {
        hs::RunStatus::Running => ("Running", ins::RunHealth::Ok),
        hs::RunStatus::Completed => ("Completed", ins::RunHealth::Ok),
        hs::RunStatus::Failed => ("Failed", ins::RunHealth::Err),
        hs::RunStatus::Stalled => ("Stalled", ins::RunHealth::Warn),
    };
    // A live run's clock is ours to compute; a finished one reported its
    // own elapsed; anything else spans its records — never an invention.
    let duration_ms = row.elapsed_ms.unwrap_or(match row.status {
        hs::RunStatus::Running => now_ms.saturating_sub(row.started_ms),
        _ => row.last_event_ms.saturating_sub(row.started_ms),
    });
    let tools = d
        .calls
        .iter()
        .map(|c| ins::ToolCall {
            tool: c.tool.clone(),
            request: c.args.clone(),
            status: match c.status {
                hs::CallStatus::Ok => ins::ToolStatus::Allowed,
                hs::CallStatus::Error => ins::ToolStatus::Failed,
                hs::CallStatus::Denied => ins::ToolStatus::Denied,
                // The run stopped between the call and its answer: still
                // waiting is the closest true word the page has.
                hs::CallStatus::Unanswered => ins::ToolStatus::Pending,
            },
            time: c.elapsed_ms.map(fmt_elapsed).unwrap_or_default(),
        })
        .collect::<Vec<_>>();
    let mut metadata: Vec<(String, String)> = vec![
        ("Session".to_string(), row.session.clone()),
        (
            "Kind".to_string(),
            match row.kind {
                hs::RunKind::Goal => "Goal".to_string(),
                hs::RunKind::Delegation => "Delegation".to_string(),
            },
        ),
        ("Model calls".to_string(), d.tokens.calls.to_string()),
        ("Input tokens".to_string(), d.tokens.input.to_string()),
        ("Output tokens".to_string(), d.tokens.output.to_string()),
        ("Cache read".to_string(), d.tokens.cache_read.to_string()),
        ("Billable tokens".to_string(), d.tokens.billable.to_string()),
    ];
    if let Some(k) = d.kicks {
        metadata.push(("Kicks".to_string(), k.to_string()));
    }
    if let Some(e) = &row.ending {
        metadata.push(("Ending".to_string(), e.clone()));
    }
    if let Some(det) = &d.ending_detail {
        metadata.push(("Ending detail".to_string(), det.clone()));
    }
    // `skipped_lines` on the incoming tree was a count; this one keeps the
    // *line numbers* (`damaged_lines`), so a page can say which lines to go and
    // look at. The metadata row wants the count, and `.len()` is it — the same
    // conversion `harness_state`'s module doc names for a caller that wants the
    // fork's number.
    if !d.damaged_lines.is_empty() {
        metadata.push((
            "Skipped lines".to_string(),
            d.damaged_lines.len().to_string(),
        ));
    }
    let events = d
        .events
        .iter()
        .map(|e| ins::Event {
            time: clock(e.at_ms, offset),
            level: match e.level {
                hs::Level::Info => ins::Level::Info,
                hs::Level::Warn => ins::Level::Warn,
                hs::Level::Error => ins::Level::Error,
            },
            text: e.line.clone(),
        })
        .collect::<Vec<_>>();
    let selected_tool = (!tools.is_empty()).then_some(0);
    Some(ins::InspectView {
        version: version.to_string(),
        run: Some(ins::RunView {
            name: row.name.clone(),
            id: row.id.clone(),
            status: status.to_string(),
            health,
            // No ceiling, no percent: the gauge shows a real zero rather
            // than a bar moving at a rate nobody measured.
            progress_pct: row
                .progress
                .as_ref()
                .and_then(|p| (p.done * 100).checked_div(p.total))
                .map(|pct| pct.min(100) as u8)
                .unwrap_or(0),
            started: clock(row.started_ms, offset),
            started_ago: hs::relative_time(row.started_ms, now_ms, offset),
            duration: fmt_duration(duration_ms),
            eta: String::new(),
            model: row.model.clone().unwrap_or_default(),
            context: String::new(),
            workspace: row.cwd.clone().unwrap_or_default(),
            branch: String::new(),
            agent: row.agent.clone().unwrap_or_default(),
            agent_runtime: String::new(),
            steps: Vec::new(),
            selected_step: 0,
            steps_done: 0,
            auto_approve: hs::gates(crate::approval::current_gate(), None)
                .map(|g| g.mode_label.to_string())
                .unwrap_or_default(),
            tools,
            selected_tool,
            metadata,
            artifacts: Vec::new(),
            events,
        }),
    })
}

/// How many step nodes the Run Graph draws before it compresses.
///
/// The canvas stacks one node per layer down the page, so a goal that ran
/// forty steps would be a column nobody can read at any terminal size. Whole
/// turns are kept, newest last, until this budget is spent; what fell off the
/// front becomes one marker node carrying the count, the `… N more` idiom the
/// dashboard and the Memory page already use.
const GRAPH_NODES: usize = 12;

/// How many rows the SELECTED NODE panel's RECENT EVENTS section gets.
const GRAPH_EVENTS: usize = 6;

/// The Run Graph's DAG for one run, out of the stored session logs.
///
/// **Honest mapping** (harness-live). Every node is a record: the goal is the
/// root, each `model_call` is a turn, each `tool_call` is a step under the
/// turn that issued it, and the ending is the sink. Every edge is the record
/// order and nothing else, so the drawing is a chain, because a single-agent
/// loop is a chain. Nothing here invents a branch.
///
/// What the log cannot back keeps the screen's unmeasured face: ETA, queue
/// depths, worker/host/pid and retries belong to a scheduler that does not
/// exist, and `parallel_branches` is 1 because the trace is sequential rather
/// than because anything was measured. `None` when no session file holds the
/// id.
fn rungraph_view_from(
    dir: &std::path::Path,
    id: &str,
    version: &str,
    now_ms: u64,
) -> Option<super::rungraph::GraphView> {
    use super::rungraph as rg;
    use crate::harness_state as hs;

    let t = hs::trace(dir, id, now_ms).ok().flatten()?;
    let offset = hs::local_offset_secs();
    let row = &t.row;
    let live = row.status == hs::RunStatus::Running;
    let root_status = match row.status {
        hs::RunStatus::Running => rg::Status::Running,
        hs::RunStatus::Completed => rg::Status::Completed,
        hs::RunStatus::Failed => rg::Status::Failed,
        // Quiet and unfinished. Nothing said it failed, so the graph must not
        // paint a cross; Skipped is the one dim word the screen has for it.
        hs::RunStatus::Stalled => rg::Status::Skipped,
    };

    // One group per model turn: the turn, then the calls it issued. Calls
    // before the first turn (a file that starts mid-run) get their own group
    // so nothing is dropped on the floor.
    let mut groups: Vec<Vec<&hs::Step>> = Vec::new();
    for step in &t.steps {
        match step {
            hs::Step::Turn(_) => groups.push(vec![step]),
            hs::Step::Call(_) => match groups.last_mut() {
                Some(g) => g.push(step),
                None => groups.push(vec![step]),
            },
        }
    }
    // Newest turns first, whole groups only, until the budget is spent. A
    // single group larger than the budget is still shown whole: half a turn
    // would be a lie about what it did.
    let mut keep_from = groups.len();
    let mut budget = GRAPH_NODES;
    while keep_from > 0 {
        let cost = groups[keep_from - 1].len();
        if cost > budget && keep_from < groups.len() {
            break;
        }
        budget = budget.saturating_sub(cost);
        keep_from -= 1;
    }
    let dropped = keep_from;

    let mut nodes: Vec<rg::Node> = Vec::new();
    let mut details: Vec<rg::NodeDetail> = Vec::new();
    let mut push = |name: String,
                    kind: rg::Kind,
                    status: rg::Status,
                    time: String,
                    parent: Option<usize>,
                    detail: rg::NodeDetail|
     -> usize {
        let i = nodes.len();
        nodes.push(rg::Node {
            id: format!("n{i}"),
            name,
            kind,
            status,
            time,
            parents: parent.into_iter().collect(),
        });
        details.push(detail);
        i
    };

    let span_ms = run_span_ms(row, now_ms);
    let mut root_inputs = vec![("Session".to_string(), row.session.clone())];
    if let Some(model) = &row.model {
        root_inputs.push(("Model".to_string(), model.clone()));
    }
    if let Some(cwd) = &row.cwd {
        root_inputs.push(("Workspace".to_string(), cwd.clone()));
    }
    let mut root_outputs = vec![
        ("Model calls".to_string(), t.tokens.calls.to_string()),
        ("Tool calls".to_string(), t.calls().to_string()),
    ];
    if let Some(tokens) = row.tokens {
        root_outputs.push(("Tokens".to_string(), tokens.to_string()));
    }
    if let Some(ending) = &row.ending {
        root_outputs.push(("Ending".to_string(), ending.clone()));
    }
    let mut last = push(
        one_line_name(&row.name),
        rg::Kind::Agent,
        root_status,
        fmt_elapsed(span_ms),
        None,
        rg::NodeDetail {
            node_type: match row.kind {
                hs::RunKind::Goal => "goal".to_string(),
                hs::RunKind::Delegation => "delegation".to_string(),
            },
            started: clock(row.started_ms, offset),
            latency: fmt_elapsed(span_ms),
            inputs: root_inputs,
            outputs: root_outputs,
            events: root_events(&t, offset),
            ..unmeasured()
        },
    );
    if dropped > 0 {
        last = push(
            format!("… {dropped} earlier turns"),
            rg::Kind::Agent,
            rg::Status::Skipped,
            "–".to_string(),
            Some(last),
            rg::NodeDetail {
                node_type: "elided".to_string(),
                outputs: vec![("Turns hidden".to_string(), dropped.to_string())],
                ..unmeasured()
            },
        );
    }
    for group in &groups[dropped..] {
        for step in group {
            let (name, kind, status, time, detail) = match step {
                // The record is appended after the provider answered, so a
                // turn in the trace is a turn that returned.
                hs::Step::Turn(turn) => (
                    format!("turn {}", turn.iteration),
                    rg::Kind::Agent,
                    rg::Status::Completed,
                    "–".to_string(),
                    rg::NodeDetail {
                        node_type: "model call".to_string(),
                        started: clock(turn.at_ms, offset),
                        outputs: vec![
                            ("Input tokens".to_string(), turn.input_tokens.to_string()),
                            ("Output tokens".to_string(), turn.output_tokens.to_string()),
                            (
                                "Stop reason".to_string(),
                                turn.stop_reason.clone().unwrap_or_default(),
                            ),
                        ],
                        ..unmeasured()
                    },
                ),
                hs::Step::Call(c) => (
                    c.tool.clone(),
                    tool_kind(&c.tool),
                    match c.status {
                        hs::CallStatus::Ok => rg::Status::Completed,
                        hs::CallStatus::Error => rg::Status::Failed,
                        // Refused, so it never ran. Not a failure of the run.
                        hs::CallStatus::Denied => rg::Status::Skipped,
                        // No answer in the file. In a live run that is the
                        // thing happening right now; in a dead one it is
                        // where the run stopped, which is not a failure.
                        hs::CallStatus::Unanswered if live => rg::Status::Running,
                        hs::CallStatus::Unanswered => rg::Status::Skipped,
                    },
                    c.elapsed_ms
                        .map(fmt_elapsed)
                        .unwrap_or_else(|| "–".to_string()),
                    rg::NodeDetail {
                        node_type: "tool call".to_string(),
                        started: clock(c.at_ms, offset),
                        // Call to answer, so it holds the approval prompt when
                        // there was one. It is not the tool's own run time.
                        latency: c.elapsed_ms.map(fmt_elapsed).unwrap_or_default(),
                        inputs: vec![("Arguments".to_string(), c.args.clone())],
                        outputs: match &c.detail {
                            Some(det) => vec![("Detail".to_string(), det.clone())],
                            None => Vec::new(),
                        },
                        ..unmeasured()
                    },
                ),
                // **The delegation arm is gone, and its absence is the
                // finding rather than a gap.** It read `Step::Node`,
                // `RunTrace::queue_waits` and a queue-depth derivation over
                // four record kinds — `node_spawn`, `node_done`, `queue_wait`,
                // `task_progress` — that `telemetry.rs` wrote on the branch
                // this came from. That module is not in this tree, nothing
                // appends any of the four, and a grep over the 156 real session
                // files on this machine found none of them
                // (`harness_state`'s module doc, §"What was left behind from
                // the fork"). Drawing a delegation graph over records no writer
                // emits could only ever be exercised by invented fixtures,
                // which is the false-receipt shape this repository has been
                // bitten by. It comes back with `telemetry.rs`, in the same
                // change, tested against what that module actually appends.
            };
            last = push(name, kind, status, time, Some(last), detail);
        }
    }
    if let Some(ending) = &row.ending {
        push(
            ending.clone(),
            rg::Kind::Output,
            match ending.as_str() {
                "done" | "answered" => rg::Status::Completed,
                _ => rg::Status::Failed,
            },
            "–".to_string(),
            Some(last),
            rg::NodeDetail {
                node_type: "ending".to_string(),
                started: clock(row.last_event_ms, offset),
                outputs: vec![("Ending".to_string(), ending.clone())],
                ..unmeasured()
            },
        );
    }

    // The tool calls are the only timed work in the trace. Their clock runs
    // from "the loop logged the call" to "the loop logged the answer", so it
    // contains an approval prompt when there was one; it is wall time and is
    // labelled as a task time, which is what the panel row asks for.
    let timed: Vec<(String, u64)> = t
        .steps
        .iter()
        .filter_map(|s| match s {
            hs::Step::Call(c) => c.elapsed_ms.map(|ms| (c.tool.clone(), ms)),
            _ => None,
        })
        .collect();
    let longest = timed
        .iter()
        .max_by_key(|(_, ms)| *ms)
        .map(|(tool, ms)| format!("{tool} ({})", fmt_elapsed(*ms)))
        .unwrap_or_else(|| "–".to_string());
    let avg = if timed.is_empty() {
        "–".to_string()
    } else {
        fmt_elapsed(timed.iter().map(|(_, ms)| *ms).sum::<u64>() / timed.len() as u64)
    };

    Some(rg::GraphView {
        version: version.to_string(),
        selected: Some(0),
        detail: details.first().cloned(),
        details,
        summary: Some(rg::RunSummary {
            run_id: row.id.clone(),
            status: root_status,
            started: clock(row.started_ms, offset),
            elapsed: fmt_duration(span_ms),
            // An agent loop has no ceiling to count down to, and there is no
            // completion-rate model here to build one from.
            eta: "–".to_string(),
            // The task file is the only thing in the process that knows what
            // the work is. A run that never opened one has no fraction, and
            // the panel falls back to the DAG's own.
            // On the row rather than the trace: this tree records a run's
            // ceiling only where `delegate.rs` writes it, and `RunRow` is where
            // it lands. The incoming `RunTrace::progress` read `task_progress`
            // records nothing here appends.
            progress: t.row.progress.map(|p| (p.done, p.total)),
        }),
        // **All five are real zeros here.** The incoming code derived the
        // first from `queue_wait` intervals — a record kind nothing in this
        // tree writes, see the delegation note in `graph_nodes`. A derived
        // depth over an empty set is zero either way; what is lost is the day
        // `telemetry.rs` returns and it stops being zero.
        queue_depths: [0, 0, 0, 0, 0],
        metrics: Some(rg::Metrics {
            // The trace is a chain, so its longest path is its length.
            critical_path: nodes.len() as u64,
            // One, because the log records one delegation at a time. Not a
            // measurement of parallelism, and never several live workers.
            parallel_branches: 1,
            longest_task: longest,
            avg_task_time: avg,
            active_workers: "–".to_string(),
        }),
        filters: rg::Filters::default(),
        nodes,
        query: String::new(),
        palette: false,
        notice: None,
        scroll: 0,
        canvas_rows: 0,
    })
}

/// The rows the Run Graph's node panel has no substrate for, on any node.
///
/// Nothing retries a step, a nested run is a future on this process's own
/// runtime rather than a worker on a host, and `telemetry.rs` records a pid
/// that is the same on every node for exactly that reason. Empty renders the
/// panel's dash, which is the honest answer.
fn unmeasured() -> super::rungraph::NodeDetail {
    super::rungraph::NodeDetail::default()
}

/// The root node's RECENT EVENTS: the refusals, the failures and the ending.
/// An unremarkable call is already a node on the canvas and does not need a
/// line here as well.
fn root_events(t: &crate::harness_state::RunTrace, offset: i64) -> Vec<super::rungraph::Event> {
    use super::rungraph as rg;
    use crate::harness_state as hs;

    let mut events: Vec<rg::Event> = Vec::new();
    for d in &t.denials {
        events.push(rg::Event {
            time: clock(d.at_ms, offset),
            level: rg::EventLevel::Info,
            text: format!("{} denied by {}: {}", d.tool, d.by, d.reason),
        });
    }
    for step in &t.steps {
        match step {
            hs::Step::Call(c) if c.status == hs::CallStatus::Error => events.push(rg::Event {
                time: clock(c.at_ms, offset),
                level: rg::EventLevel::Info,
                text: format!(
                    "{} failed: {}",
                    c.tool,
                    c.detail.clone().unwrap_or_default()
                ),
            }),
            // No delegation arm: nothing writes `node_spawn`/`node_done` in
            // this tree. See the note in `graph_nodes`.
            _ => {}
        }
    }
    if let Some(ending) = &t.row.ending {
        events.push(rg::Event {
            time: clock(t.row.last_event_ms, offset),
            level: rg::EventLevel::Debug,
            text: format!("goal {ending} after {} model calls", t.tokens.calls),
        });
    }
    events.sort_by(|a, b| a.time.cmp(&b.time));
    let older = events.len().saturating_sub(GRAPH_EVENTS);
    events.split_off(older)
}

/// A node's [`rungraph::Kind`] from the tool's name.
///
/// **The enum is about the glyph, not about a taxonomy of tools.** It has one
/// variant for "a tool ran" and the mock spends it on shell, file write and
/// git alike, so read/search/edit/write/bash/web all land on
/// [`rungraph::Kind::Tool`]: splitting them would mean claiming a distinction
/// the screen cannot draw. Memory is the one exception, because the page has a
/// glyph for the memory store and Emma's memory tools are that store.
fn tool_kind(tool: &str) -> super::rungraph::Kind {
    let lower = tool.to_ascii_lowercase();
    if lower.contains("memory") || lower.contains("wiki") {
        super::rungraph::Kind::Memory
    } else {
        super::rungraph::Kind::Tool
    }
}

/// A goal's first line, cut to what a node box can show without the layout
/// widening every other box to match.
fn one_line_name(name: &str) -> String {
    const NODE_NAME: usize = 28;
    let first = name.lines().next().unwrap_or("").trim();
    if first.chars().count() <= NODE_NAME {
        return first.to_string();
    }
    let kept: String = first.chars().take(NODE_NAME - 1).collect();
    format!("{kept}…")
}

/// A wall-clock `HH:MM:SS` in the local offset — the mock's event times.
/// The LANGUAGE SERVERS card's rows, plus the enabled keys this build does not
/// know, from settings.json and the filesystem.
///
/// **No process is started.** `server::presence` is a `stat` of every candidate
/// entry point and of its launcher on `PATH`; `server::resolve`, which is the
/// truth, executes candidates with `--version` and cannot run here, because
/// this is called from `toggle_settings` on the input thread and a spawn there
/// is a freeze with no way out. What that costs is honesty about the word: the
/// row says `found`, and `NOTICE_LSP_FOUND` says found means on disk.
fn lsp_rows(stored: &crate::settings::Settings) -> (Vec<super::settings::LspRow>, Vec<String>) {
    use super::settings::{LspFound, LspRow};
    use emma_tools_lsp::lang;
    use emma_tools_lsp::server::{self, Presence};

    // One row per language in the table. `found` is a claim about a file on
    // disk and never about a server that starts: `server::presence` spawns
    // nothing, because this runs on the input thread from `toggle_settings`
    // and a spawn there is a freeze with no way out. `server::resolve` is the
    // truth and costs a process per candidate.
    let enabled: Vec<String> = stored.lsp.enabled.clone().unwrap_or_else(|| {
        lang::DEFAULT_ENABLED
            .iter()
            .map(|s| s.to_string())
            .collect()
    });

    let rows = lang::LANGUAGES
        .iter()
        .map(|l| LspRow {
            label: l.label.to_string(),
            key: l.key.to_string(),
            enabled: enabled.iter().any(|k| k.trim().eq_ignore_ascii_case(l.key)),
            found: match server::presence(l) {
                Presence::Found { .. } => LspFound::Found,
                Presence::NeedsLauncher { needs, .. } => LspFound::Needs(needs.to_string()),
                Presence::Absent => LspFound::Absent,
            },
            network: l.network,
        })
        .collect();

    // Keys in `lsp.enabled` that name no language here. Kept and reported
    // rather than rejected, so a settings file written by a newer build does
    // not disable the languages this one does know.
    let unknown = enabled
        .iter()
        .filter(|k| lang::by_key(k).is_none())
        .cloned()
        .collect();

    (rows, unknown)
}

/// The TOOL PERMISSIONS card's rows: the rules really in force for this
/// project, the file a new one is written to, and whether the look happened at
/// all.
///
/// **Read directly rather than through `Harness::load`.** Booting a harness
/// from the input thread reads every skill, agent and command file under the
/// root; this needs two JSON documents. It is also the one shape that can
/// answer honestly when the harness would refuse to boot — a malformed
/// `settings.local.json` fails `Harness::load` outright, and a Settings screen
/// that could not open because of it would be the worst possible time to be
/// unable to look at the file.
///
/// The order is the order the rules are consulted — deny, then ask, then
/// allow — matching `PermissionBlock::into_entries`, so the card reads in
/// precedence order and a reader does not have to know the precedence to read
/// the card correctly.
///
/// **Nothing here writes.** See `settings::perm_rows` for why that is the
/// design and not an unfinished half.
fn permission_rows() -> (Vec<super::settings::PermRow>, Option<String>, bool) {
    use super::settings::PermRow;

    let Ok(cwd) = std::env::current_dir() else {
        return (Vec::new(), None, false);
    };
    let Ok(root) = emma_harness::discover_from(
        &cwd,
        std::env::var_os(emma_harness::ROOT_ENV).map(std::path::PathBuf::from),
    ) else {
        // No harness root: not an error and not a lie. There are no project
        // rules because there is no project, which the card says.
        return (Vec::new(), None, true);
    };
    let file = crate::permissions::file_for(&root);
    let mut rows: Vec<PermRow> = Vec::new();
    // The spine file first (`settings.json` beside the harness), then the
    // local file — the same two documents `emma_harness::read_permissions`
    // merges, in the same order.
    for source in [root.join("settings.json"), file.clone()] {
        let Ok(raw) = std::fs::read_to_string(&source) else {
            continue;
        };
        let Ok(doc) = serde_json::from_str::<serde_json::Value>(&raw) else {
            // A malformed file is reported as a row rather than skipped: the
            // whole point of the card is that a rule nobody can see is a rule
            // nobody remembers granting, and an unreadable file is the
            // strongest possible case of that.
            rows.push(PermRow {
                rule: format!("{} is malformed", source.display()),
                verdict: "unread",
            });
            continue;
        };
        for (list, verdict) in [("deny", "deny"), ("ask", "ask"), ("allow", "allow")] {
            let Some(items) = doc["permissions"][list].as_array() else {
                continue;
            };
            for item in items {
                if let Some(rule) = item.as_str() {
                    rows.push(PermRow {
                        rule: rule.to_string(),
                        verdict,
                    });
                }
            }
        }
    }
    // Deny before ask before allow across *both* files, which is how they are
    // consulted: a deny in the spine outranks an allow in the local file.
    rows.sort_by_key(|r| match r.verdict {
        "unread" => 0,
        "deny" => 1,
        "ask" => 2,
        _ => 3,
    });
    (rows, Some(file.display().to_string()), true)
}

/// The Ollama host the Test Connection ping aims at — `ollama.rs`'s own
/// resolution: `OLLAMA_HOST`, bare spellings accepted, else localhost:11434.
fn ollama_host() -> String {
    std::env::var("OLLAMA_HOST")
        .ok()
        .filter(|h| !h.trim().is_empty())
        .unwrap_or_else(|| "localhost:11434".to_string())
}

/// A bounded TCP connect to `host` (a URL or a bare `host:port`): can the
/// Ollama port be reached at all. Bounded because this runs on the input
/// thread under the frame lock — 400ms of worst case, never a hang.
fn ping(host: &str) -> bool {
    use std::net::{TcpStream, ToSocketAddrs};
    let bare = host
        .trim_start_matches("http://")
        .trim_start_matches("https://")
        .trim_end_matches('/');
    let addr = if bare.contains(':') {
        bare.to_string()
    } else {
        format!("{bare}:11434")
    };
    let Ok(mut addrs) = addr.to_socket_addrs() else {
        return false;
    };
    addrs.any(|a| TcpStream::connect_timeout(&a, std::time::Duration::from_millis(400)).is_ok())
}

/// `YYYYMMDD-HHMMSS` in UTC, for the export filename. Civil-from-days is
/// Howard Hinnant's algorithm; no clock crate is worth this one name.
fn timestamp(now: std::time::SystemTime) -> String {
    let secs = now
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = (secs / 86_400) as i64;
    let (h, m, s) = ((secs / 3600) % 24, (secs / 60) % 60, secs % 60);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mth = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if mth <= 2 { y + 1 } else { y };
    format!("{y:04}{mth:02}{d:02}-{h:02}{m:02}{s:02}")
}

fn clock(at_ms: u64, offset_secs: i64) -> String {
    let s = ((at_ms / 1000) as i64 + offset_secs).rem_euclid(86_400);
    format!("{:02}:{:02}:{:02}", s / 3600, (s % 3600) / 60, s % 60)
}

/// A duration for the header strip: `2h 18m 24s` shapes.
fn fmt_duration(ms: u64) -> String {
    let s = ms / 1000;
    if s >= 3600 {
        format!("{}h {}m {}s", s / 3600, (s % 3600) / 60, s % 60)
    } else if s >= 60 {
        format!("{}m {}s", s / 60, s % 60)
    } else {
        format!("{s}s")
    }
}

/// A tool call's elapsed for the five-column TIME cell: `0.5s`, `12s`, `3m`.
fn fmt_elapsed(ms: u64) -> String {
    if ms < 10_000 {
        format!("{:.1}s", ms as f64 / 1000.0)
    } else if ms < 600_000 {
        format!("{}s", ms / 1000)
    } else {
        format!("{}m", ms / 60_000)
    }
}

/// The last path component, for the status bar's ENV cell. Local rather than
/// borrowed from `render.rs`, whose copy is private and whose signature may
/// move with the cell renderers.
/// What a click on the sidebar asks for. One variant today; an enum rather
/// than a bool so the second control does not have to rewrite the routing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidebarAction {
    /// The `[+]` on the SESSIONS header: start a fresh conversation.
    NewSession,
}

/// The command the `[+]` submits, and the notice that says it happened.
///
/// **`/clear` is what Emma actually has.** There is no session fork and no
/// second log file behind this control: `/clear` starts a fresh conversation
/// inside the running session and keeps the grants the user has already given.
/// A `[+]` that promised a new *session* would be promising a feature, so the
/// notice says what happened in the words the command's own help uses — and
/// the SESSIONS list does not gain a row, because no new session was made.
pub const NEW_SESSION_COMMAND: &str = "/clear";
pub const NEW_SESSION_NOTICE: &str = "new conversation — grants kept";

fn within(rect: Rect, col: u16, row: u16) -> bool {
    col >= rect.x && col < rect.x + rect.width && row >= rect.y && row < rect.y + rect.height
}

/// Where the SESSIONS header's `[+]` sits, given the sidebar's own rectangle.
///
/// **Geometry, not a question asked of `sidebar.rs`.** That module renders and
/// reports nothing back, so this re-derives the three facts its header row is
/// built from: the pane is bordered, so its content starts one cell in on both
/// axes; the header is the content's first row; and the affordance is
/// right-aligned in it, dropped whole when it does not fit rather than clipped
/// to a `[+` that is not a control. Those are the rules in `sidebar::header`,
/// and the test that both agree is the one that reads the rect off a real
/// paint.
pub fn new_session_hit(sidebar: Rect) -> Option<Rect> {
    // The same floor `sidebar::render` refuses to draw below.
    if sidebar.width < 3 || sidebar.height < 3 {
        return None;
    }
    let width = sidebar::COLLAPSE_HINT.chars().count() as u16;
    let inner_w = sidebar.width - 2;
    if inner_w < width {
        return None;
    }
    // `sidebar::header` leaves its right pad after the affordance, and drops
    // the pad whole — never the glyph — when the two together will not fit.
    // The same two rules, in the same order, or the rectangle is not where the
    // glyph is.
    let pad = if width + sidebar::HEADER_RIGHT_PAD > inner_w {
        0
    } else {
        sidebar::HEADER_RIGHT_PAD
    };
    Some(Rect::new(
        sidebar.x + 1 + inner_w - width - pad,
        sidebar.y + 1,
        width,
        1,
    ))
}

/// What a left-button press at `(col, row)` means, given where the sidebar is.
///
/// `None` for every other cell of the pane, deliberately: a sidebar where one
/// control works and the rest of the surface swallows clicks is worse than one
/// where the pointer passes through.
pub fn sidebar_click(
    sidebar: Rect,
    col: u16,
    row: u16,
    prompt_pending: bool,
) -> Option<SidebarAction> {
    // While a question is on screen the pointer belongs to it, the same rule
    // `pane_key` applies to the keyboard.
    if prompt_pending {
        return None;
    }
    new_session_hit(sidebar)
        .filter(|rect| within(*rect, col, row))
        .map(|_| SidebarAction::NewSession)
}

/// The three facts SESSIONS is built from, remembered between refreshes.
#[derive(Debug, Clone)]
struct SessionScope {
    /// The session directory, when the transcript path named one.
    dir: Option<std::path::PathBuf>,
    /// The directory the run is in — the filter, not decoration.
    cwd: String,
    /// This run's session id, so its row is found and marked.
    current: String,
}

/// What the current run is called before its first goal names it.
///
/// The tail of the id, by the same argument [`harness_state::sessions_for`]
/// makes for a session whose goals carried no text: the front of every id on
/// the machine is identical, and the sidebar truncates from the right.
fn current_name(id: &str) -> String {
    let count = id.chars().count();
    id.chars().skip(count.saturating_sub(12)).collect()
}

/// The Memory page's data, read from the project wiki on the spot.
///
/// This is where plan M0 (the page) and M1 (the store) meet: MemorySummary is
/// the store's seam struct, MemoryView is the page's, and this function is the
/// entire coupling. Conversation memory, the index, and auto-save render their
/// honest empty states because those stages (M2/M3) do not exist yet — the
/// page's own law is that it never claims what the store cannot back.
fn memory_view_from(cwd: &str) -> super::memory::MemoryView {
    use super::memory::{MemRow, MemoryView, Recent, Source};
    use crate::memory::{Category, Entry, Filter, Wiki};

    let empty = MemoryView {
        version: concat!("v", env!("CARGO_PKG_VERSION")).to_string(),
        embedding_model: "index-first (none)".to_string(),
        ..MemoryView::default()
    };
    // ⚠ A FAILURE IS NOT AN EMPTY STORE. `empty` is the shape of a wiki with
    // nothing in it, so returning it bare for a read error tells the reader the
    // opposite of what happened — the F32 arm the text pages carried and this
    // one dropped. The notice is the page's own honesty channel and it names
    // the store, so the Harness page's identical failure is distinguishable
    // from this one.
    let unreadable = MemoryView {
        notice: Some(super::memory::NOTICE_UNREADABLE.to_string()),
        ..empty.clone()
    };
    let Ok(wiki) = Wiki::project(std::path::Path::new(cwd)) else {
        return unreadable;
    };
    let Ok(s) = wiki.view() else {
        return unreadable;
    };
    // `Category::ALL` and the page's `CATEGORIES` share one order; the index
    // is the entire mapping between the two layers.
    let cat_index = |c: Category| Category::ALL.iter().position(|a| *a == c).unwrap_or(0);
    let row = |e: &Entry| MemRow {
        slug: e.slug.clone(),
        title: e.title.clone(),
        category: cat_index(e.category),
        created: e.created.clone(),
        pinned: e.pinned,
    };
    // The full live listing feeds the ALL MEMORIES sub-view; the summary's
    // `recent` is capped by design and cannot.
    let all = wiki
        .list(Filter::default())
        .map(|l| l.pages.iter().map(|p| row(&Entry::from(p))).collect())
        .unwrap_or_default();
    MemoryView {
        recent: s
            .recent
            .iter()
            .map(|e| Recent {
                source: Source::Doc,
                text: e.title.clone(),
                time: e.created.clone(),
                slug: e.slug.clone(),
            })
            .collect(),
        pinned: s.pinned.iter().map(row).collect(),
        total_memories: s.counts.total as u64,
        category_counts: Category::ALL.map(|c| s.counts.of(c) as u64),
        all,
        ..empty
    }
}

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

    // -----------------------------------------------------------------------
    // The Settings screen
    // -----------------------------------------------------------------------

    fn settings_bar() -> statusbar::Bar {
        statusbar::Bar {
            mode: "ASSIST".into(),
            model: "llama3:8b".into(),
            env: "dev".into(),
            ctx_used: 68,
            ctx_max: 100,
            total_used: 12_842,
            total_max: 500_000,
            up: Some(8_128),
            down: Some(4_714),
            elapsed: None,
        }
    }

    fn draw_settings(w: u16, h: u16) -> Vec<String> {
        let mut app = App::new((w, h));
        app.toggle_settings();
        assert!(app.settings_open());
        let mut v = view();
        v.status.model = "llama3:8b".into();
        v.status.cwd = "~/projects/research".into();
        let area = Rect::new(0, 0, w, h);
        let mut buf = Buffer::empty(area);
        let cursor = app.render(area, &mut buf, &v, &settings_bar());
        assert!(
            cursor.is_none(),
            "the settings screen has nothing to type into"
        );
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

    /// The whole mock at once: the eight numbered cards in order, on the
    /// full-screen frame at a standard size.
    #[test]
    fn the_settings_screen_shows_the_eight_cards_in_order() {
        let rows = draw_settings(161, 75);
        let all = rows.join("\n");
        let mut at = 0;
        for h in [
            "1. MODEL PROVIDER",
            "2. CONTEXT LIMITS",
            "3. APPEARANCE",
            "4. KEYBINDINGS",
            "5. MEMORY PREFERENCES",
            "6. TOOL PERMISSIONS",
            "7. ENVIRONMENT",
            "8. SAVE & RESET",
        ] {
            let pos = all.find(h).unwrap_or_else(|| panic!("{h} missing"));
            // Reading order: 1,2 then 3,4 … — each pair strictly below the last.
            if h.starts_with('3') || h.starts_with('5') || h.starts_with('7') {
                assert!(pos > at, "{h} out of order");
            }
            at = at.max(pos);
        }
        assert!(all.contains("Settings"), "title missing");
        assert!(
            all.contains(concat!("v", env!("CARGO_PKG_VERSION"))),
            "version missing"
        );
    }

    /// The sidebar while Settings is open: the mock's tool rows, the Settings
    /// row carrying the `>` marker and the highlight band.
    #[test]
    fn the_settings_row_in_the_sidebar_is_the_selected_one() {
        let rows = draw_settings(161, 75);
        let row = rows
            .iter()
            .find(|r| r.contains("Settings") && r.contains(","))
            .expect("no Settings tool row");
        assert!(
            row.contains("> "),
            "Settings row lacks the selection marker: {row:?}"
        );
        // And the key letters are the mock's, right of their names.
        for (name, key) in [("Shell", "s"), ("Code", "c"), ("File Browser", "f")] {
            let r = rows.iter().find(|r| r.contains(name)).unwrap();
            let n = r.find(name).unwrap();
            let k = r.rfind(key).unwrap();
            assert!(k > n, "{name}'s key letter is not right of the name");
        }
    }

    // -----------------------------------------------------------------------
    // SESSIONS
    // -----------------------------------------------------------------------

    /// A session file with one goal in `cwd`, timestamped `at_ms`.
    fn session_file(dir: &std::path::Path, id: &str, cwd: &str, at_ms: u64, goal: &str) -> String {
        let record = serde_json::json!({
            "kind": "goal",
            "at_ms": at_ms,
            "session_id": id,
            "text": goal,
            "cwd": cwd,
            "model": "claude-sonnet-5",
        });
        let path = dir.join(format!("{id}.jsonl"));
        std::fs::write(&path, format!("{record}\n")).expect("write session");
        path.display().to_string()
    }

    const T0: u64 = 1_700_000_000_000;

    #[test]
    fn sessions_lists_the_other_runs_in_this_repo_and_not_only_this_one() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        session_file(
            dir.path(),
            "sess-1700000000000-1",
            "/repo",
            T0,
            "the older one",
        );
        session_file(
            dir.path(),
            "sess-1700000000000-2",
            "/elsewhere",
            T0 + 86_400_000,
            "another project",
        );
        session_file(
            dir.path(),
            "sess-1700000000000-3",
            "/repo",
            T0 + 172_800_000,
            "the newer one",
        );
        let mine = session_file(
            dir.path(),
            "sess-1700000000000-9",
            "/repo",
            T0 + 259_200_000,
            "what I am doing now",
        );

        let mut app = App::new((100, 30));
        app.set_identity("sess-1700000000000-9", &mine, "/repo", &skin());

        let names: Vec<&str> = app.side.sessions.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(
            names,
            ["what I am doing now", "the newer one", "the older one"],
            "the other sessions in this repo are missing"
        );
        assert!(
            app.side.sessions[0].selected,
            "the current run is not marked"
        );
        assert!(
            app.side.sessions[1..].iter().all(|r| !r.selected),
            "more than one row is selected"
        );
        assert!(
            app.side.sessions.iter().all(|r| !r.trailing.is_empty()),
            "a row has no time column: {:?}",
            app.side.sessions
        );
    }

    /// The first thing that happens in a run is `set_identity`, and it happens
    /// before the first goal — so the current session's file is empty and the
    /// cwd filter cannot see it. It is still the row the reader is sitting on.
    #[test]
    fn the_current_session_is_listed_before_it_has_recorded_a_goal() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        session_file(
            dir.path(),
            "sess-1700000000000-1",
            "/repo",
            T0,
            "earlier work",
        );
        let mine = dir.path().join("sess-1787712345678-48282.jsonl");
        std::fs::write(&mine, "").expect("write session");

        let mut app = App::new((100, 30));
        app.set_identity(
            "sess-1787712345678-48282",
            &mine.display().to_string(),
            "/repo",
            &skin(),
        );

        assert_eq!(app.side.sessions.len(), 2);
        assert_eq!(app.side.sessions[0].name, "345678-48282");
        assert!(app.side.sessions[0].selected);
        assert_eq!(app.side.sessions[1].name, "earlier work");
    }

    #[test]
    fn the_list_stops_at_the_sidebars_ceiling() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        for i in 0..20 {
            session_file(
                dir.path(),
                &format!("sess-17000000000{i:02}-1"),
                "/repo",
                T0 + i * 1_000,
                &format!("goal {i}"),
            );
        }
        let mine = session_file(
            dir.path(),
            "sess-1700000009999-1",
            "/repo",
            T0 + 999_000,
            "now",
        );

        let mut app = App::new((100, 30));
        app.set_identity("sess-1700000009999-1", &mine, "/repo", &skin());
        assert_eq!(
            app.side.sessions.len(),
            crate::harness_state::SIDEBAR_SESSIONS
        );
        assert!(app.side.sessions[0].selected);
    }

    /// No session directory to read — a `--no-session` run, or a transcript
    /// path that is not a file. The current run is still a row.
    #[test]
    fn an_unreadable_session_directory_leaves_the_current_run_as_the_one_row() {
        let mut app = App::new((100, 30));
        app.set_identity("sess-1787712345678-48282", "", "/repo", &skin());
        assert_eq!(app.side.sessions.len(), 1);
        assert_eq!(app.side.sessions[0].name, "345678-48282");
        assert!(app.side.sessions[0].selected);
    }

    // -----------------------------------------------------------------------
    // The [+] on the SESSIONS header
    // -----------------------------------------------------------------------

    /// A sidebar 30 columns wide at the window's left edge. Its inner area
    /// starts one in from the border, so the `[+]` ends at column 28.
    const SIDE: Rect = Rect {
        x: 0,
        y: 0,
        width: 30,
        height: 20,
    };

    #[test]
    fn the_affordance_is_the_last_three_columns_of_the_headers_row() {
        // **Two columns in from the inner edge, not flush against it.**
        // `sidebar::header` leaves `HEADER_RIGHT_PAD` after the affordance, and
        // the incoming hit-test did not — so every click landed two columns
        // to the right of the glyph. `the_paint_records_where_the_affordance_landed`
        // reads the real paint and is what caught it; these coordinates follow
        // that, not the other way round.
        let hit = new_session_hit(SIDE).expect("no affordance");
        assert_eq!((hit.x, hit.y, hit.width, hit.height), (24, 1, 3, 1));
    }

    #[test]
    fn a_click_on_the_affordance_asks_for_a_new_conversation() {
        assert_eq!(
            sidebar_click(SIDE, 25, 1, false),
            Some(SidebarAction::NewSession)
        );
        // Both ends of it, because a control whose right column is dead is a
        // control people miss.
        assert_eq!(
            sidebar_click(SIDE, 24, 1, false),
            Some(SidebarAction::NewSession)
        );
        assert_eq!(
            sidebar_click(SIDE, 26, 1, false),
            Some(SidebarAction::NewSession)
        );
    }

    #[test]
    fn a_click_anywhere_else_in_the_sidebar_does_nothing() {
        // The row below it — the first session.
        assert_eq!(sidebar_click(SIDE, 25, 2, false), None);
        // The header's own text.
        assert_eq!(sidebar_click(SIDE, 2, 1, false), None);
        // One column short of the affordance, and the pad beside it — the
        // pad is air, not part of the control.
        assert_eq!(sidebar_click(SIDE, 23, 1, false), None);
        assert_eq!(sidebar_click(SIDE, 27, 1, false), None);
        // Outside the pane entirely: the chat side of the window.
        assert_eq!(sidebar_click(SIDE, 60, 1, false), None);
    }

    /// While a question is on screen the keyboard is for answering it, and so
    /// is the pointer: a click that starts a fresh conversation underneath an
    /// unanswered approval is a click nobody meant.
    #[test]
    fn a_pending_prompt_suppresses_the_affordance() {
        assert_eq!(sidebar_click(SIDE, 25, 1, true), None);
    }

    #[test]
    fn a_sidebar_too_narrow_to_hold_the_affordance_has_none_to_click() {
        // The affordance is dropped whole rather than clipped, so there is
        // nothing to hit — not a two-column control.
        let narrow = Rect::new(0, 0, 4, 20);
        assert_eq!(new_session_hit(narrow), None);
        assert_eq!(sidebar_click(narrow, 1, 1, false), None);
        // Collapsed: no pane at all.
        assert_eq!(new_session_hit(Rect::new(0, 0, 0, 20)), None);
    }

    /// The one test that can catch this file and `sidebar.rs` disagreeing:
    /// the recorded rectangle is checked against the cells the glyph actually
    /// landed on, so a change to the header's layout fails here rather than
    /// leaving a control that is one column off.
    #[test]
    fn the_paint_records_where_the_affordance_landed() {
        let mut app = App::new((100, 30));
        let (rows, _) = draw(&mut app, &view(), 100, 30);
        let hit = app.sessions_add.expect("nothing recorded");
        assert_eq!(hit.height, 1);
        assert_eq!(hit.width, sidebar::COLLAPSE_HINT.chars().count() as u16);

        let painted: Vec<char> = rows[usize::from(hit.y)].chars().collect();
        let at = painted
            .windows(3)
            .position(|w| w.iter().collect::<String>() == sidebar::COLLAPSE_HINT)
            .expect("no [+] on the header row");
        assert_eq!(at as u16, hit.x, "the rect is not where the glyph is");

        assert!(app.new_session_clicked(hit.x, hit.y, false));
        assert!(!app.new_session_clicked(hit.x, hit.y + 1, false));
    }

    #[test]
    fn a_collapsed_sidebar_records_no_affordance_at_all() {
        let mut app = App::new((100, 30));
        app.toggle_sidebar(100);
        let (_, _) = draw(&mut app, &view(), 100, 30);
        assert_eq!(app.sessions_add, None);
        assert!(!app.new_session_clicked(27, 1, false));
    }

    // -----------------------------------------------------------------------
    // One composition, every window
    //
    // The owner's instruction is structural: every screen — chat, settings,
    // whatever comes next — draws the sidebar and the bottom bar through the
    // one seam in `layout::compose`. These tests read the chrome off each
    // screen's rendered frame, so a screen that wanders off the seam fails
    // here before anyone notices a bare window.
    // -----------------------------------------------------------------------

    fn chrome_is_on(rows: &[String], screen: &str) {
        let all = rows.join("\n");
        for section in ["SESSIONS", "TOOLS", "QUICK HELP"] {
            assert!(all.contains(section), "{screen}: sidebar lacks {section}");
        }
        let bar_rows = rows[rows.len().saturating_sub(3)..].join("\n");
        for cell in ["MODE", "ENV"] {
            assert!(
                bar_rows.contains(cell),
                "{screen}: bottom bar lacks {cell}: {bar_rows:?}"
            );
        }
    }

    /// The chat screen's chrome, off the seam.
    #[test]
    fn the_chat_screen_draws_the_sidebar_and_the_status_bar() {
        let mut app = App::new((120, 40));
        let (rows, _) = draw(&mut app, &view(), 120, 40);
        chrome_is_on(&rows, "chat");
    }

    /// The sidebar's TOOLS and QUICK HELP are the mock's canonical sections
    /// on the chat screen too — not a Settings-only swap. Chat selects no
    /// tool row; the rows themselves are `sidebar::tool_rows`'s.
    #[test]
    fn the_tools_and_quick_help_are_the_mocks_on_every_screen() {
        let mut app = App::new((120, 40));
        let (rows, _) = draw(&mut app, &view(), 120, 40);
        let all = rows.join("\n");
        // Harness replaced Data Explorer in the roster (owner, 2026-08-26 —
        // `sidebar::TOOLS` holds the ruling and the reason).
        for tool in ["Shell", "Code", "File Browser", "Harness", "Settings"] {
            assert!(all.contains(tool), "chat sidebar lacks the {tool} tool row");
        }
        // **`Ctrl + k` was the incoming assertion and this tree does not bind
        // it.** QUICK HELP here is derived from `super::bindings::CHAT`, whose
        // rows carry the chord they mean and are driven through the real
        // decoders; the panel it replaced was six hand-typed pairs, and
        // the fork inventory counted four of that
        // panel's six keys doing nothing — one of them `Ctrl+k`, for a
        // binding that is `Ctrl-U`. Asserting a chord the table actually holds
        // is what makes this test about the panel rather than about a literal.
        assert!(
            all.contains("Ctrl+B"),
            "chat sidebar lacks the mock's QUICK HELP"
        );
        let settings_row = rows
            .iter()
            .find(|r| r.contains("Settings") && r.contains(","))
            .expect("no Settings tool row");
        assert!(
            !settings_row.contains("> "),
            "chat selects no tool row: {settings_row:?}"
        );
    }

    /// The settings screen's chrome, off the same seam.
    #[test]
    fn the_settings_screen_draws_the_sidebar_and_the_status_bar() {
        let rows = draw_settings(120, 40);
        chrome_is_on(&rows, "settings");
    }

    /// A configured status program replaces the built-in cells on *every*
    /// screen — the settings screen used to render the built-in bar past it,
    /// which is exactly the drift one composition path exists to prevent.
    #[test]
    fn a_configured_status_program_replaces_the_bar_on_the_settings_screen_too() {
        let mut app = App::new((120, 40));
        app.toggle_settings();
        let mut v = view();
        v.custom_status = Some("main ~ 12% ctx".into());
        let area = Rect::new(0, 0, 120, 40);
        let mut buf = Buffer::empty(area);
        app.render(area, &mut buf, &v, &settings_bar());
        let rows: Vec<String> = (0..40)
            .map(|y| {
                (0..120)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect();
        let bar_rows = rows[37..].join("\n");
        assert!(
            bar_rows.contains("main ~ 12% ctx"),
            "the configured status line is missing from the settings screen: {bar_rows:?}"
        );
        assert!(
            !bar_rows.contains("MODE"),
            "the built-in cells were drawn over the configured line"
        );
    }

    /// The bottom bar under the settings screen: all five segments.
    #[test]
    fn the_bottom_bar_shows_all_five_segments() {
        let rows = draw_settings(161, 75);
        let bar_rows = rows[rows.len().saturating_sub(3)..].join("\n");
        for seg in [
            "MODE",
            "◇ ASSIST",
            "TOKENS",
            "CONTEXT",
            "ENV",
            "llama3:8b",
            // **`? help` is the incoming bar's last cell and this tree's is
            // `/help · /exit`.** `term/statusbar.rs` was not taken — it
            // is the hardened one — and the two disagree about how the help
            // cell reads. A difference for the owner, not something to make the
            // bar say by editing it here.
            "/help",
        ] {
            assert!(
                bar_rows.contains(seg),
                "bottom bar lacks {seg}: {bar_rows:?}"
            );
        }
    }

    /// The block gauge fills in proportion to the context level: ceilinged,
    /// never full below the cap — `statusbar::filled`'s contract, read off the
    /// rendered frame.
    #[test]
    fn the_context_gauge_fills_proportionally() {
        // 68/100 over ten segments: ceil(6.8) = 7 full, 3 empty.
        //
        // The full glyph is this tree's ‘▊’ and not the incoming
        // ‘█’; `term/statusbar.rs` was not taken. The
        // *proportion* is what this test is about and it is unchanged — a
        // gauge that filled wrongly would still fail here.
        let rows = draw_settings(161, 75);
        let all = rows.join("");
        assert_eq!(all.matches('▊').count(), 7, "filled segments");
        assert_eq!(all.matches('░').count(), 3, "empty segments");
    }

    /// Nothing on the settings frame overruns its width, mock size included.
    #[test]
    fn no_settings_row_is_ever_wider_than_the_window() {
        for (w, h) in [
            (161, 75),
            (120, 40),
            (100, 30),
            (80, 24),
            (60, 18),
            (40, 10),
        ] {
            for row in draw_settings(w, h) {
                assert!(
                    super::super::render::cols(&row) <= usize::from(w),
                    "row overruns at {w}x{h}: {row:?}"
                );
            }
        }
    }

    #[test]
    #[ignore]
    fn dump_the_settings_screen() {
        for row in draw_settings(161, 75) {
            println!("{row}");
        }
    }

    /// The same service for the Memory page, which until 2026-09-05 nobody
    /// could look at without running Emma.
    ///
    /// Seeded rather than empty: an empty wiki renders the honest empty state,
    /// which the assertions already cover and which shows a reader nothing
    /// about the row layout, the counts or the category column. The store is
    /// `memory_rig`'s tempdir, so this reads no wiki belonging to anyone.
    #[test]
    #[ignore]
    fn dump_the_memory_screen() {
        let (_dir, _wiki, mut app) = memory_rig(&[
            ("Deploy window", Category::Facts),
            ("Reviews", Category::Workflows),
        ]);
        for row in draw(&mut app, &view(), 161, 75).0 {
            println!("{row}");
        }
    }

    /// And for the Harness dashboard, off `harness_rig`'s session directory —
    /// two runs, one finished and one still going, so the six cards are drawn
    /// with data rather than with their empty states.
    #[test]
    #[ignore]
    fn dump_the_harness_screen() {
        let (_dir, mut app, _now) = harness_rig();
        for row in draw(&mut app, &view(), 161, 75).0 {
            println!("{row}");
        }
    }

    /// `,` toggles: open shows the title, close restores the chat frame.
    #[test]
    fn toggling_settings_off_restores_the_chat_frame() {
        let mut app = App::new((120, 40));
        app.toggle_settings();
        app.toggle_settings();
        assert!(!app.settings_open());
        let v = view();
        let area = Rect::new(0, 0, 120, 40);
        let mut buf = Buffer::empty(area);
        let cursor = app.render(area, &mut buf, &v, &bar());
        assert!(cursor.is_some(), "the input box did not come back");
    }

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
            rows.iter().any(|r| r.contains("src")),
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

    // -----------------------------------------------------------------------
    // The chat pane's scrollbar
    // -----------------------------------------------------------------------

    /// Fill the transcript with `n` one-row notes.
    fn fill(app: &mut App, n: usize) {
        let skin = skin();
        for i in 0..n {
            app.push_block(vec![Line::from(Span::raw(format!("line {i}")))], &skin);
        }
    }

    /// One column of the chat pane, as symbols.
    fn column(buf: &Buffer, x: u16, area: Rect) -> Vec<String> {
        (area.y..area.bottom())
            .map(|y| buf[(x, y)].symbol().to_string())
            .collect()
    }

    fn painted(app: &mut App, v: &View, w: u16, h: u16) -> Buffer {
        let area = Rect::new(0, 0, w, h);
        let mut buf = Buffer::empty(area);
        app.render(area, &mut buf, v, &bar());
        buf
    }

    /// An affordance for a state: nothing to scroll, no bar. The pane keeps
    /// every column it had, which is also what stops a short transcript from
    /// re-wrapping the moment one more line arrives.
    #[test]
    fn the_scrollbar_appears_only_when_the_transcript_outgrows_the_pane() {
        let mut app = App::new((120, 40));
        let v = view();
        fill(&mut app, 3);
        let _ = painted(&mut app, &v, 120, 40);
        assert_eq!(
            app.scrollbar, None,
            "a pane that holds everything drew a bar"
        );
        let wide = app.chat_rect;

        fill(&mut app, 200);
        let buf = painted(&mut app, &v, 120, 40);
        let x = app
            .scrollbar
            .expect("an overflowing transcript drew no bar");
        assert_eq!(
            x,
            wide.right() - 1,
            "the bar took a column that is not the pane's last"
        );
        assert_eq!(
            app.chat_rect.width,
            wide.width - 1,
            "the message column did not pay for the bar"
        );
        // Every cell of that column belongs to the bar — track or thumb, and
        // nothing of the transcript underneath it.
        let col = column(&buf, x, app.chat_rect);
        assert!(
            col.iter().all(|c| c == "\u{2502}" || c == "\u{2588}"),
            "something else is painted in the bar column: {col:?}"
        );
        assert!(
            col.iter().any(|c| c == "\u{2588}"),
            "the bar has no thumb: {col:?}"
        );
    }

    /// The bar reads the transcript's own offset: at the tail the thumb is at
    /// the bottom, and Home puts it at the top. Drawn, not asserted on the
    /// geometry function — this is the wiring, and the wiring is what an
    /// inverted offset breaks.
    #[test]
    fn the_thumb_follows_the_transcripts_own_scroll_offset() {
        let mut app = App::new((120, 40));
        let v = view();
        fill(&mut app, 200);
        let buf = painted(&mut app, &v, 120, 40);
        let x = app.scrollbar.unwrap();
        let col = column(&buf, x, app.chat_rect);
        assert_eq!(
            col.last().map(String::as_str),
            Some("\u{2588}"),
            "following, the thumb is not at the bottom: {col:?}"
        );

        app.transcript.to_top();
        let buf = painted(&mut app, &v, 120, 40);
        let col = column(&buf, x, app.chat_rect);
        assert_eq!(
            col.first().map(String::as_str),
            Some("\u{2588}"),
            "at Home the thumb is not at the top: {col:?}"
        );
        assert_ne!(
            col.last().map(String::as_str),
            Some("\u{2588}"),
            "the thumb spans the whole track: {col:?}"
        );
    }

    /// ASCII console, no colour: the bar is still there and still a bar. The
    /// distinction rides the glyphs, per `render.rs`'s standing rule.
    #[test]
    fn the_scrollbar_degrades_to_ascii_with_the_console() {
        let mut app = App::new((120, 40));
        let mut v = View::new(Skin::new(
            Palette::new(Level::None),
            super::super::render::ASCII,
        ));
        v.status.model = "llama3:8b".into();
        fill(&mut app, 200);
        let buf = painted(&mut app, &v, 120, 40);
        let x = app.scrollbar.expect("no bar on an ASCII console");
        let col = column(&buf, x, app.chat_rect);
        assert!(col.iter().all(|c| c.is_ascii()), "{col:?}");
        assert!(col.iter().any(|c| c == "#"), "no thumb: {col:?}");
        assert!(col.iter().any(|c| c == "|"), "no track: {col:?}");
    }

    // -----------------------------------------------------------------------
    // The copy notice
    // -----------------------------------------------------------------------

    /// It reaches the pane's bottom-right corner, and it leaves on the next
    /// thing the reader does. Nothing here waits for a clock: a notice that
    /// expired on its own would need a repaint nobody asked for.
    #[test]
    fn the_notice_paints_in_the_corner_and_leaves_with_the_next_gesture() {
        let mut app = App::new((120, 40));
        let v = view();
        fill(&mut app, 200);

        app.notice_sent(128);
        let buf = painted(&mut app, &v, 120, 40);
        let pane = app.chat_rect;
        let last: String = (pane.x..pane.right())
            .map(|x| buf[(x, pane.bottom() - 1)].symbol())
            .collect();
        assert!(last.contains("128 chars (~32 tokens)"), "{last:?}");

        // A key — the frame calls this on every one of them.
        assert!(app.notice_clear(), "the notice was not there to clear");
        assert!(!app.notice_clear(), "a second clear claimed a notice");
        let buf = painted(&mut app, &v, 120, 40);
        let last: String = (pane.x..pane.right())
            .map(|x| buf[(x, pane.bottom() - 1)].symbol())
            .collect();
        assert!(
            !last.contains("sent"),
            "the notice outlived the keypress: {last:?}"
        );

        // The next selection retires it too: the reader is copying again, and
        // a receipt for the last drag standing over this one is a lie about
        // which text is on the clipboard.
        app.notice_sent(128);
        assert!(app.selection_begin(pane.x + 3, pane.y + 1));
        assert!(
            !app.notice_clear(),
            "a new selection left the old receipt up"
        );
        // And so does a press on the scrollbar.
        app.notice_sent(128);
        let x = app.scrollbar.expect("no bar");
        assert!(app.bar_press(x, pane.y));
        assert!(!app.notice_clear(), "the bar left the old receipt up");
    }

    // -----------------------------------------------------------------------
    // The scrollbar as a control
    // -----------------------------------------------------------------------

    /// The bar's column is not the pane, so a press on it cannot be a
    /// selection — and the reason it cannot is arithmetic, not a rule anybody
    /// has to remember: `chat_rect` had that column taken out of it before
    /// either hit-test existed.
    #[test]
    fn a_press_in_the_bars_column_never_starts_a_selection() {
        let mut app = App::new((120, 40));
        let v = view();
        fill(&mut app, 200);
        let _ = painted(&mut app, &v, 120, 40);
        let x = app.scrollbar.expect("no bar to press");
        let row = app.chat_rect.y + 5;

        assert!(app.bar_press(x, row), "the bar refused a press on itself");
        assert!(
            !app.has_selection(),
            "a press on the bar started a selection"
        );
        assert!(
            !app.selection_begin(x, row),
            "the bar's column is inside the selectable pane"
        );
        // One column left is the transcript, and there a press is a drag.
        assert!(
            !app.bar_press(x - 1, row),
            "the bar claimed the message column"
        );
        assert!(app.selection_begin(x - 1, row));
        // A press on the bar also dismisses whatever was highlighted, the way
        // a press anywhere off the pane does.
        assert!(app.bar_press(x, row));
        assert!(!app.has_selection());
    }

    /// The track pages and the thumb grabs. The page is the keys' page —
    /// [`App::page`] — because a bar that scrolled by some private number
    /// would put two step sizes in one pane.
    #[test]
    fn the_track_pages_and_the_thumb_only_grabs() {
        let mut app = App::new((120, 40));
        let v = view();
        fill(&mut app, 200);
        let _ = painted(&mut app, &v, 120, 40);
        let x = app.scrollbar.unwrap();
        let pane = app.chat_rect;
        let page = app.page();

        // Following: the thumb is at the foot of the track, so the top of the
        // track is above it.
        assert!(app.bar_press(x, pane.y));
        assert_eq!(
            app.transcript.scroll_offset(),
            page,
            "the track did not page"
        );
        assert!(!app.bar_dragging(), "a press on the track began a drag");

        // The thumb, wherever it now is: the view does not move until the
        // pointer does.
        let (top, _) = transcript::thumb(
            app.transcript.total_rows(),
            pane.height,
            app.transcript.scroll_offset(),
        )
        .unwrap();
        let before = app.transcript.scroll_offset();
        assert!(app.bar_press(x, pane.y + top));
        assert!(app.bar_dragging(), "a press on the thumb began no drag");
        assert_eq!(
            app.transcript.scroll_offset(),
            before,
            "the press moved the view before the pointer did"
        );

        // And the track below the thumb pages back towards the tail.
        app.bar_release();
        assert!(app.bar_press(x, pane.bottom() - 1));
        assert_eq!(app.transcript.scroll_offset(), before - page.min(before));
    }

    /// A drag moves the view while the button is down and stops moving it
    /// when the button comes up. The release matters as much as the drag: a
    /// grab left latched turns every later pointer motion into a scroll.
    #[test]
    fn dragging_the_thumb_scrolls_and_releasing_stops_it() {
        let mut app = App::new((120, 40));
        let v = view();
        fill(&mut app, 200);
        let _ = painted(&mut app, &v, 120, 40);
        let x = app.scrollbar.unwrap();
        let pane = app.chat_rect;

        assert!(
            app.bar_press(x, pane.bottom() - 1),
            "the thumb is at the tail"
        );
        assert!(app.bar_dragging());
        assert!(app.bar_drag(pane.y), "a drag to the top row moved nothing");
        assert_eq!(
            transcript::thumb(
                app.transcript.total_rows(),
                pane.height,
                app.transcript.scroll_offset()
            )
            .unwrap()
            .0,
            0,
            "the thumb did not follow the pointer to the top of the track"
        );
        assert!(!app.transcript.is_following());

        // Off the bottom of the pane: an ordinary drag, clamped, and it is the
        // tail — which re-latches the follow exactly as End does.
        assert!(app.bar_drag(pane.bottom() + 20));
        assert_eq!(app.transcript.scroll_offset(), 0);
        assert!(app.transcript.is_following());

        assert!(app.bar_release(), "the release did not end a live drag");
        assert!(!app.bar_dragging());
        assert!(!app.bar_release(), "a second release claimed a drag");
        assert!(
            !app.bar_drag(pane.y),
            "the pointer still scrolls after the button came up"
        );
        assert!(
            app.transcript.is_following(),
            "a released drag moved the view"
        );
    }

    /// No bar, no control. A transcript that fits drew nothing in that column
    /// and a press there must fall through to whatever else wants it.
    #[test]
    fn there_is_nothing_to_press_when_there_is_no_bar() {
        let mut app = App::new((120, 40));
        let v = view();
        fill(&mut app, 3);
        let _ = painted(&mut app, &v, 120, 40);
        assert_eq!(app.scrollbar, None);
        assert!(!app.bar_press(app.chat_rect.right() - 1, app.chat_rect.y));
        assert!(!app.bar_dragging());
    }

    // -----------------------------------------------------------------------
    // Selecting text in the chat pane
    // -----------------------------------------------------------------------

    /// Cell rows, the way a painted pane hands them over: one symbol per
    /// column.
    fn cells(rows: &[&str]) -> Vec<Vec<String>> {
        rows.iter()
            .map(|r| r.chars().map(|c| c.to_string()).collect())
            .collect()
    }

    /// One row, part of a line: exactly the cells the pointer crossed, head
    /// cell included — a drag that ends on a character has selected it.
    #[test]
    fn a_selection_inside_one_row_is_the_cells_it_covers() {
        let c = cells(&["You    fix the tests", "                    "]);
        assert_eq!(selected_text(&c, (0, 7), (0, 9)), "fix");
        assert_eq!(selected_text(&c, (0, 0), (0, 2)), "You");
    }

    /// More than one row is read in reading order, not as a rectangle: the
    /// first row runs to its end, the last stops at the pointer. A block
    /// selection would cut a wrapped sentence into columns, which is not what
    /// the reader is pointing at.
    #[test]
    fn a_multi_row_selection_reads_in_reading_order() {
        let c = cells(&["Emma   one two", "       three  ", "       four   "]);
        assert_eq!(
            selected_text(&c, (0, 7), (2, 10)),
            "one two\n       three\n       four"
        );
    }

    /// Dragging up and dragging down select the same cells. The anchor is
    /// where the button went down, not where the smaller coordinate is.
    #[test]
    fn dragging_backwards_selects_the_same_text() {
        let c = cells(&["one two", "three  ", "four   "]);
        let down = selected_text(&c, (0, 4), (2, 3));
        let up = selected_text(&c, (2, 3), (0, 4));
        assert_eq!(down, up);
        assert_eq!(down, "two\nthree\nfour");
    }

    /// Trailing blanks are the pane's padding, not the reader's text — and
    /// coordinates past the end clamp rather than panic, because a drag that
    /// leaves the pane is a normal drag.
    #[test]
    fn padding_is_trimmed_and_coordinates_out_of_range_clamp() {
        let c = cells(&["hello      ", "           "]);
        assert_eq!(selected_text(&c, (0, 0), (0, 200)), "hello");
        assert_eq!(selected_text(&c, (0, 0), (99, 99)), "hello");
        assert_eq!(selected_text(&[], (0, 0), (3, 3)), "");
        // A row of nothing but padding selects nothing at all.
        assert_eq!(selected_text(&c, (1, 0), (1, 9)), "");
    }

    /// Styling is not text. A wide glyph's continuation cell carries no
    /// symbol of its own and must not become a space in the copy.
    #[test]
    fn only_the_text_survives_a_selection_across_styled_cells() {
        let c = vec![vec!["\u{6f22}".to_string(), String::new(), "x".to_string()]];
        assert_eq!(selected_text(&c, (0, 0), (0, 2)), "\u{6f22}x");
    }

    /// The drag, end to end, against a painted frame: press inside the pane,
    /// drag, and the cells between are highlighted while everything else is
    /// left alone. The text that comes off it is what was on those rows.
    #[test]
    fn a_drag_inside_the_chat_pane_highlights_and_yields_its_text() {
        let mut app = App::new((120, 40));
        let v = view();
        app.push_user("fix the tests", &v.skin);
        let _ = painted(&mut app, &v, 120, 40);
        let pane = app.chat_rect;

        assert!(
            app.selection_begin(pane.x + 7, pane.y),
            "the press missed the pane"
        );
        app.selection_extend(pane.x + 9, pane.y);
        let buf = painted(&mut app, &v, 120, 40);
        let reversed = |x: u16, y: u16| {
            buf[(x, y)]
                .style()
                .add_modifier
                .contains(ratatui::style::Modifier::REVERSED)
        };
        for x in pane.x + 7..=pane.x + 9 {
            assert!(reversed(x, pane.y), "cell {x} is not highlighted");
        }
        assert!(!reversed(pane.x + 6, pane.y), "the highlight bled left");
        assert!(!reversed(pane.x + 10, pane.y), "the highlight bled right");
        assert_eq!(app.selection_text().chars().count(), 3);
    }

    /// A press outside the pane is not a selection, and it clears whatever
    /// was selected — the sidebar, the input box and the bar are not the
    /// transcript.
    #[test]
    fn a_press_outside_the_chat_pane_clears_the_selection() {
        let mut app = App::new((120, 40));
        let v = view();
        app.push_user("fix the tests", &v.skin);
        let _ = painted(&mut app, &v, 120, 40);
        let pane = app.chat_rect;
        assert!(app.selection_begin(pane.x + 7, pane.y));
        assert!(
            !app.selection_begin(0, 0),
            "a click on the sidebar started a selection"
        );
        assert_eq!(app.selection_text(), "");
        assert!(!app.selection_clear(), "nothing was left to clear");
    }

    /// Settings and Memory are out of scope, and out of scope means the mouse
    /// cannot reach them: no pane rectangle, no leftover highlight from the
    /// chat screen underneath.
    #[test]
    fn no_other_screen_can_be_selected_over() {
        let mut app = App::new((120, 40));
        let v = view();
        app.push_user("fix the tests", &v.skin);
        let _ = painted(&mut app, &v, 120, 40);
        let pane = app.chat_rect;
        assert!(app.selection_begin(pane.x + 7, pane.y));

        app.toggle_settings();
        let _ = painted(&mut app, &v, 120, 40);
        assert!(!app.has_selection(), "the highlight survived into Settings");
        assert_eq!(app.scrollbar, None);
        assert!(
            !app.selection_begin(pane.x + 7, pane.y),
            "a click started a selection on the Settings screen"
        );
    }

    // -- the memory key seam, against a tempdir wiki (plan M5) ---------------

    use super::super::memory::{Focus, PageMode, NOTICE_M2};
    use crate::memory::{Archived, Category, Filter, Wiki};
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    /// A throwaway wiki and an App with the Memory page open on it.
    fn memory_rig(seed: &[(&str, Category)]) -> (tempfile::TempDir, Wiki, App) {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let wiki = Wiki::project(dir.path()).expect("wiki");
        for (title, cat) in seed {
            wiki.create(title, *cat, "test", "body").expect("seed");
        }
        let mut app = App::new((161, 50));
        app.toggle_memory(&dir.path().to_string_lossy());
        (dir, wiki, app)
    }

    fn tap(app: &mut App, code: KeyCode) {
        assert!(
            app.memory_key(KeyEvent::from(code)),
            "the open page refused {code:?}"
        );
    }

    #[test]
    fn pin_unpin_archive_round_trip_through_memory_key() {
        let (_dir, wiki, mut app) = memory_rig(&[
            ("Deploy window", Category::Facts),
            ("Reviews", Category::Workflows),
        ]);
        assert_eq!(app.memory.as_ref().unwrap().recent.len(), 2);

        // Tab lands on the first recent row; [p] pins it — on disk.
        tap(&mut app, KeyCode::Tab);
        tap(&mut app, KeyCode::Char('p'));
        let pinned_view: Vec<String> = app
            .memory
            .as_ref()
            .unwrap()
            .pinned
            .iter()
            .map(|r| r.slug.clone())
            .collect();
        assert_eq!(pinned_view.len(), 1, "the view does not show the pin");
        let on_disk = wiki.list(Filter::default().pinned(true)).unwrap().pages;
        assert_eq!(on_disk.len(), 1, "the pin did not reach disk");
        assert_eq!(on_disk[0].slug, pinned_view[0], "view and disk disagree");

        // [u] on the same selected recent row unpins it again.
        tap(&mut app, KeyCode::Char('u'));
        assert!(app.memory.as_ref().unwrap().pinned.is_empty());
        assert!(wiki
            .list(Filter::default().pinned(true))
            .unwrap()
            .pages
            .is_empty());

        // [x] archives it: gone from the live view, present in archive/.
        tap(&mut app, KeyCode::Char('x'));
        let v = app.memory.as_ref().unwrap();
        assert_eq!(v.recent.len(), 1, "the archived row still shows");
        assert_eq!(v.total_memories, 1);
        assert_eq!(
            wiki.list(Filter::default().archived(Archived::Only))
                .unwrap()
                .pages
                .len(),
            1
        );
    }

    #[test]
    fn the_add_flow_creates_a_page_on_disk_and_says_so() {
        let (_dir, wiki, mut app) = memory_rig(&[]);
        tap(&mut app, KeyCode::Char('a'));
        for c in "Emma remembers".chars() {
            tap(&mut app, KeyCode::Char(c));
        }
        // Tab cycles facts -> workflows before creating.
        tap(&mut app, KeyCode::Tab);
        tap(&mut app, KeyCode::Enter);
        let pages = wiki.list(Filter::default()).unwrap().pages;
        assert_eq!(pages.len(), 1, "no page created");
        assert_eq!(pages[0].title, "Emma remembers");
        assert_eq!(
            pages[0].front.category,
            Category::Workflows,
            "Tab did not move the category"
        );
        let v = app.memory.as_ref().unwrap();
        assert_eq!(v.total_memories, 1, "the view was not rebuilt from disk");
        assert!(
            v.notice.as_deref().unwrap_or_default().contains("Added"),
            "no confirmation notice: {:?}",
            v.notice
        );
        assert_eq!(v.query, "", "the typed title lingers in the box");
    }

    #[test]
    fn the_query_and_search_answers_are_honestly_deferred_to_m2() {
        let (_dir, _wiki, mut app) = memory_rig(&[]);
        app.memory.as_mut().unwrap().focus = Some(Focus::QueryBox);
        for c in "what do you know".chars() {
            tap(&mut app, KeyCode::Char(c));
        }
        tap(&mut app, KeyCode::Enter);
        assert_eq!(
            app.memory.as_ref().unwrap().notice.as_deref(),
            Some(NOTICE_M2)
        );
        // And [s] from the action bar says the same thing.
        app.memory.as_mut().unwrap().focus = None;
        app.memory.as_mut().unwrap().notice = None;
        tap(&mut app, KeyCode::Char('s'));
        assert_eq!(
            app.memory.as_ref().unwrap().notice.as_deref(),
            Some(NOTICE_M2)
        );
    }

    #[test]
    fn an_unpin_in_manage_pinned_leaves_the_list_live() {
        let (_dir, wiki, mut app) = memory_rig(&[
            ("Name Alan", Category::Facts),
            ("Timezone", Category::Facts),
        ]);
        wiki.pin("name-alan").unwrap();
        wiki.pin("timezone").unwrap();
        app.memory_key(KeyEvent::from(KeyCode::Char('R'))); // pick up the pins
        app.memory.as_mut().unwrap().focus = Some(Focus::PinnedManage);
        tap(&mut app, KeyCode::Enter);
        assert_eq!(app.memory.as_ref().unwrap().mode, PageMode::ManagePinned);
        tap(&mut app, KeyCode::Char('u'));
        let v = app.memory.as_ref().unwrap();
        assert_eq!(v.mode, PageMode::ManagePinned, "unpin left the sub-view");
        assert_eq!(
            v.pinned.len(),
            1,
            "the unpinned row did not leave the live list"
        );
        assert_eq!(
            wiki.list(Filter::default().pinned(true))
                .unwrap()
                .pages
                .len(),
            1
        );
    }

    #[test]
    fn chords_fall_through_and_a_closed_page_consumes_nothing() {
        let (_dir, _wiki, mut app) = memory_rig(&[]);
        let alt_m = KeyEvent::new(KeyCode::Char('m'), KeyModifiers::ALT);
        assert!(!app.memory_key(alt_m), "Alt+m must stay the shell's toggle");
        assert!(
            app.memory_key(KeyEvent::from(KeyCode::Char('z'))),
            "a plain key escaped the page"
        );
        let mut closed = App::new((161, 50));
        assert!(!closed.memory_key(KeyEvent::from(KeyCode::Char('p'))));
    }
    // -- the harness key seam, against a tempdir session log ------------------

    use serde_json::json;

    fn hrec(at_ms: u64, kind: &str, mut body: serde_json::Value) -> serde_json::Value {
        let obj = body.as_object_mut().expect("record body");
        obj.insert("kind".into(), json!(kind));
        obj.insert("at_ms".into(), json!(at_ms));
        body
    }

    fn hgoal(at: u64, text: &str) -> serde_json::Value {
        hrec(
            at,
            "goal",
            json!({
                "session_id": "sess-h", "text": text, "opening": text,
                "cwd": "/repo", "instructions_hash": "abc",
                "tool_schema_hash": "def", "model": "claude-sonnet-5",
                "done_check": "marker_claim",
            }),
        )
    }

    /// A session directory holding one finished goal (with a real tool call
    /// and a denied one) and one still-running goal, and an App with the
    /// Harness page open on it. Timestamps sit just behind the real clock so
    /// the second goal reads as Running, not Stalled.
    fn harness_rig() -> (tempfile::TempDir, App, u64) {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        let t0 = now - 60_000;
        let records = [
            hgoal(t0, "Fix the flaky retry test"),
            hrec(
                t0 + 100,
                "tool_call",
                json!({ "turn_id": "turn-1", "id": "toolu_1", "tool": "Read",
                        "args": { "path": "src/lib.rs" } }),
            ),
            hrec(
                t0 + 600,
                "tool_result",
                json!({ "turn_id": "turn-1", "id": "toolu_1", "tool": "Read",
                        "block": { "type": "tool_result", "tool_use_id": "toolu_1",
                                   "content": "fn main() {}" },
                        "truncated": false }),
            ),
            hrec(
                t0 + 700,
                "tool_call",
                json!({ "turn_id": "turn-1", "id": "toolu_2", "tool": "Write",
                        "args": { "path": "src/lib.rs" } }),
            ),
            hrec(
                t0 + 800,
                "denied",
                json!({ "turn_id": "turn-1", "id": "toolu_2", "by": "user",
                        "reason": "the user said no" }),
            ),
            hrec(
                t0 + 1_000,
                "model_call",
                json!({ "turn_id": "turn-1", "iteration": 1, "stop_reason": "end_turn",
                        "input_tokens": 100, "output_tokens": 20,
                        "cache_creation_input_tokens": 0, "cache_read_input_tokens": 5,
                        "billable_total_tokens": 125, "cost_tokens": 125,
                        "goal_total_so_far": 125 }),
            ),
            hrec(
                t0 + 2_000,
                "goal_finished",
                json!({ "ending": "done", "detail": serde_json::Value::Null,
                        "tokens": 125, "iterations": 1, "kicks": 0,
                        "elapsed_ms": 2_000 }),
            ),
            hgoal(now - 5_000, "Investigate the timeout"),
        ];
        let lines: Vec<String> = records.iter().map(|r| r.to_string()).collect();
        std::fs::write(
            dir.path().join("sess-h.jsonl"),
            format!("{}\n", lines.join("\n")),
        )
        .expect("write session");
        let mut app = App::new((161, 50));
        app.harness_dir = dir.path().to_path_buf();
        app.toggle_harness("/repo");
        (dir, app, now)
    }

    // -----------------------------------------------------------------------
    // The Run Graph, derived from one run's records
    // -----------------------------------------------------------------------

    /// A session directory holding exactly `records`, and an App with the
    /// Harness page open on it and its Run Graph mounted for the newest run.
    /// `home` points at the tempdir too, so nothing here reads the real
    /// `~/.emma` for settings either.
    fn graph_rig(records: &[serde_json::Value]) -> (tempfile::TempDir, App) {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let lines: Vec<String> = records.iter().map(|r| r.to_string()).collect();
        std::fs::write(
            dir.path().join("sess-g.jsonl"),
            format!("{}\n", lines.join("\n")),
        )
        .expect("write session");
        let mut app = App::new((161, 60));
        app.harness_dir = dir.path().to_path_buf();
        app.home = Some(dir.path().to_path_buf());
        app.toggle_harness("/repo");
        app.harness_key(KeyEvent::from(KeyCode::Char('g')));
        (dir, app)
    }

    fn graph_of(app: &App) -> &super::super::rungraph::GraphView {
        app.harness
            .as_ref()
            .and_then(|v| v.graph.as_ref())
            .expect("the Run Graph is mounted")
    }

    fn tcall(at: u64, turn: u32, id: &str, tool: &str) -> serde_json::Value {
        hrec(
            at,
            "tool_call",
            json!({ "turn_id": format!("turn-{turn}"), "id": id, "tool": tool,
                    "args": { "path": "src/lib.rs" } }),
        )
    }

    fn tok(at: u64, turn: u32, id: &str, tool: &str) -> serde_json::Value {
        hrec(
            at,
            "tool_result",
            json!({ "turn_id": format!("turn-{turn}"), "id": id, "tool": tool,
                    "block": { "type": "tool_result", "tool_use_id": id, "content": "ok" },
                    "truncated": false }),
        )
    }

    fn mcall(at: u64, iteration: u32) -> serde_json::Value {
        hrec(
            at,
            "model_call",
            json!({ "turn_id": format!("turn-{iteration}"), "iteration": iteration,
                    "stop_reason": "tool_use", "input_tokens": 100, "output_tokens": 20,
                    "cache_creation_input_tokens": 0, "cache_read_input_tokens": 5,
                    "billable_total_tokens": 125, "cost_tokens": 125,
                    "goal_total_so_far": 125 }),
        )
    }

    fn now_ms() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_millis() as u64
    }

    /// The contract: one node per record that is a step, in record order, and
    /// a status that is the record's own answer. A tool that failed is Failed,
    /// a tool that was refused never ran and is Skipped, and the run's ending
    /// is the sink.
    #[test]
    fn a_finished_run_becomes_a_chain_whose_statuses_are_the_records() {
        use super::super::rungraph::{Kind, Status};
        let t0 = now_ms() - 60_000;
        let (_dir, app) = graph_rig(&[
            hgoal(t0, "Fix the flaky retry test"),
            mcall(t0 + 100, 1),
            tcall(t0 + 200, 1, "toolu_1", "Read"),
            tok(t0 + 700, 1, "toolu_1", "Read"),
            tcall(t0 + 800, 1, "toolu_2", "Bash"),
            hrec(
                t0 + 900,
                "tool_failed",
                json!({ "turn_id": "turn-1", "id": "toolu_2", "tool": "Bash",
                        "detail": "exited with 1" }),
            ),
            tcall(t0 + 950, 1, "toolu_3", "Write"),
            hrec(
                t0 + 960,
                "denied",
                json!({ "turn_id": "turn-1", "id": "toolu_3", "by": "user",
                        "reason": "the user said no" }),
            ),
            mcall(t0 + 1_000, 2),
            hrec(
                t0 + 2_000,
                "goal_finished",
                json!({ "ending": "done", "detail": serde_json::Value::Null,
                        "tokens": 125, "iterations": 2, "kicks": 0,
                        "elapsed_ms": 2_000 }),
            ),
        ]);
        let g = graph_of(&app);
        let shape: Vec<(&str, Kind, Status)> = g
            .nodes
            .iter()
            .map(|n| (n.name.as_str(), n.kind, n.status))
            .collect();
        assert_eq!(
            shape,
            vec![
                ("Fix the flaky retry test", Kind::Agent, Status::Completed),
                ("turn 1", Kind::Agent, Status::Completed),
                ("Read", Kind::Tool, Status::Completed),
                ("Bash", Kind::Tool, Status::Failed),
                ("Write", Kind::Tool, Status::Skipped),
                ("turn 2", Kind::Agent, Status::Completed),
                ("done", Kind::Output, Status::Completed),
            ],
        );
        // Sequential, and only sequential: every node's one parent is the
        // node before it, which is the whole of what the log records.
        for (i, n) in g.nodes.iter().enumerate().skip(1) {
            assert_eq!(n.parents, vec![i - 1], "node {i} invented an edge");
        }
        assert_eq!(
            g.nodes[0].parents,
            Vec::<usize>::new(),
            "the goal is the root"
        );
        let m = g.metrics.as_ref().expect("metrics");
        assert_eq!(m.parallel_branches, 1, "a chain has one branch");
        assert_eq!(m.critical_path, g.nodes.len() as u64);
        assert_eq!(m.active_workers, "–", "no record names a worker");
        assert_eq!(g.queue_depths, [0; 5], "no queue exists to be deep");
        let s = g.summary.as_ref().expect("summary");
        assert_eq!(s.status, Status::Completed);
        assert_eq!(s.eta, "–", "an agent loop has no ceiling to count down to");
        assert!(!s.started.is_empty() && !s.elapsed.is_empty());
        let d = g.detail.as_ref().expect("detail");
        assert_eq!(d.node_type, "goal");
        assert!(
            d.retries.is_empty() && d.worker.is_empty() && d.pid.is_empty(),
            "scheduler rows must stay unmeasured"
        );
        let events: Vec<&str> = d.events.iter().map(|e| e.text.as_str()).collect();
        assert!(
            events.iter().any(|e| e.contains("Bash failed")),
            "{events:?}"
        );
        assert!(
            events.iter().any(|e| e.contains("Write denied by user")),
            "{events:?}"
        );
        assert!(events.iter().any(|e| e.contains("goal done")), "{events:?}");
    }

    /// A live run's unanswered call is the thing happening right now, and is
    /// the one node the page may call Running.
    #[test]
    fn a_live_runs_unanswered_call_is_the_running_node() {
        use super::super::rungraph::Status;
        let t0 = now_ms() - 2_000;
        let (_dir, app) = graph_rig(&[
            hgoal(t0, "Investigate the timeout"),
            mcall(t0 + 100, 1),
            tcall(t0 + 200, 1, "toolu_1", "Bash"),
        ]);
        let g = graph_of(&app);
        assert_eq!(g.nodes.len(), 3, "goal, turn, call");
        assert_eq!(g.nodes[0].status, Status::Running, "the goal is live");
        assert_eq!(g.nodes[2].name, "Bash");
        assert_eq!(g.nodes[2].status, Status::Running);
        assert!(
            g.nodes
                .iter()
                .all(|n| n.status != Status::Pending && n.status != Status::Queued),
            "Pending and Queued describe a scheduler that does not exist",
        );
    }

    /// The same call in a run that went quiet is not Running and not Failed:
    /// nothing said it failed and nothing says it lives.
    #[test]
    fn a_dead_runs_unanswered_call_is_skipped_rather_than_failed() {
        use super::super::rungraph::Status;
        let t0 = now_ms() - 10 * crate::harness_state::STALE_AFTER_MS;
        let (_dir, app) = graph_rig(&[
            hgoal(t0, "killed mid-flight"),
            mcall(t0 + 100, 1),
            tcall(t0 + 200, 1, "toolu_1", "Bash"),
        ]);
        let g = graph_of(&app);
        assert_eq!(g.nodes[0].status, Status::Skipped, "stalled, not failed");
        assert_eq!(g.nodes[2].status, Status::Skipped);
    }

    /// A long run compresses to whole turns with the count of what fell off,
    /// the `… N more` idiom the dashboard and the Memory page use.
    #[test]
    fn a_long_run_keeps_the_recent_turns_and_counts_the_rest() {
        use super::super::rungraph::Status;
        let t0 = now_ms() - 600_000;
        let mut records = vec![hgoal(t0, "a long one")];
        for turn in 1..=10u32 {
            let at = t0 + u64::from(turn) * 1_000;
            records.push(mcall(at, turn));
            records.push(tcall(at + 100, turn, &format!("toolu_{turn}a"), "Read"));
            records.push(tok(at + 200, turn, &format!("toolu_{turn}a"), "Read"));
            records.push(tcall(at + 300, turn, &format!("toolu_{turn}b"), "Grep"));
            records.push(tok(at + 400, turn, &format!("toolu_{turn}b"), "Grep"));
        }
        records.push(hrec(
            t0 + 20_000,
            "goal_finished",
            json!({ "ending": "done", "detail": serde_json::Value::Null,
                    "tokens": 900, "iterations": 10, "kicks": 0, "elapsed_ms": 20_000 }),
        ));
        let (_dir, app) = graph_rig(&records);
        let g = graph_of(&app);
        // Whole turns only, and the marker is the one node that is not a
        // record: it carries the count of the turns it stands for.
        let marker = g
            .nodes
            .iter()
            .find(|n| n.name.starts_with('…'))
            .expect("the compressed run must say what it hid");
        assert_eq!(marker.name, "… 6 earlier turns");
        assert_eq!(marker.status, Status::Skipped);
        let turns = g
            .nodes
            .iter()
            .filter(|n| n.name.starts_with("turn "))
            .count();
        assert_eq!(turns, 4, "the budget keeps four three-node turns");
        assert_eq!(
            g.nodes[1].name, marker.name,
            "the marker sits under the root"
        );
        assert_eq!(
            g.nodes[2].name, "turn 7",
            "the kept turns are the recent ones"
        );
        assert_eq!(g.nodes.last().expect("sink").name, "done");
    }

    /// No sessions in this repo is the honest empty state, unchanged.
    #[test]
    fn a_repo_with_no_runs_still_gets_the_no_run_graph() {
        let (_dir, app) = graph_rig(&[]);
        let g = graph_of(&app);
        assert!(g.nodes.is_empty() && g.summary.is_none() && g.metrics.is_none());
        assert!(app.harness.as_ref().expect("page").graph_id.is_none());
    }

    /// RUNTIME STATUS's live rows are wiring, not honesty: a run with records
    /// on disk knows all five, and an empty cell there is a defect.
    #[test]
    fn the_runtime_card_is_fed_by_the_newest_run_in_this_repo() {
        let t0 = now_ms() - 60_000;
        let (_dir, app) = graph_rig(&[
            hgoal(t0, "Fix the flaky retry test"),
            mcall(t0 + 100, 1),
            hrec(
                t0 + 2_000,
                "goal_finished",
                json!({ "ending": "done", "detail": serde_json::Value::Null,
                        "tokens": 125, "iterations": 1, "kicks": 0, "elapsed_ms": 2_000 }),
            ),
        ]);
        let r = &app.harness.as_ref().expect("page").runtime;
        assert_eq!(
            r.model, "claude-sonnet-5",
            "the goal record names the model"
        );
        assert!(!r.provider.is_empty(), "provider must resolve");
        assert!(!r.started.is_empty(), "started is the first record's clock");
        assert!(!r.uptime.is_empty(), "uptime is the run's own span");
        assert_eq!(r.health, Some(true), "a run that ended done is healthy");
        assert!(!r.runtime.is_empty() && !r.binary.is_empty() && !r.version.is_empty());
    }

    /// The palette is a mode: `:` hands it the keyboard, typing echoes, and
    /// Enter answers. `clear` is one of the commands the page can honestly
    /// serve, so it acts rather than apologising.
    #[test]
    fn the_run_graphs_command_bar_takes_keys_and_answers() {
        let t0 = now_ms() - 60_000;
        let (_dir, mut app) = graph_rig(&[
            hgoal(t0, "Fix the flaky retry test"),
            mcall(t0 + 100, 1),
            hrec(
                t0 + 2_000,
                "goal_finished",
                json!({ "ending": "done", "detail": serde_json::Value::Null,
                        "tokens": 125, "iterations": 1, "kicks": 0, "elapsed_ms": 2_000 }),
            ),
        ]);
        // Unfocused, a letter is still a page key and must not be typed.
        app.harness_key(KeyEvent::from(KeyCode::Down));
        assert_eq!(graph_of(&app).selected, Some(1), "↑/↓ moves the selection");
        assert!(graph_of(&app).query.is_empty());

        app.harness_key(KeyEvent::from(KeyCode::Char(':')));
        for c in "node n2".chars() {
            app.harness_key(KeyEvent::from(KeyCode::Char(c)));
        }
        assert_eq!(
            graph_of(&app).query,
            "node n2",
            "typing must echo in the bar"
        );
        app.harness_key(KeyEvent::from(KeyCode::Enter));
        let g = graph_of(&app);
        assert_eq!(g.selected, Some(2), "the command acted");
        assert!(g.query.is_empty(), "a run command clears the bar");
        assert!(!g.palette, "Enter hands the keyboard back to the page");
        assert_eq!(g.notice.as_deref(), Some("selected n2"));

        // A command with no backend says which part is missing.
        app.harness_key(KeyEvent::from(KeyCode::Char(':')));
        for c in "run".chars() {
            app.harness_key(KeyEvent::from(KeyCode::Char(c)));
        }
        app.harness_key(KeyEvent::from(KeyCode::Enter));
        assert_eq!(
            graph_of(&app).notice.as_deref(),
            Some(super::super::rungraph::NOTICE_RUN),
        );

        // And an unknown one names what does work rather than going dead.
        app.harness_key(KeyEvent::from(KeyCode::Char(':')));
        for c in "wat".chars() {
            app.harness_key(KeyEvent::from(KeyCode::Char(c)));
        }
        app.harness_key(KeyEvent::from(KeyCode::Enter));
        let notice = graph_of(&app).notice.clone().expect("a notice");
        assert!(
            notice.contains(super::super::rungraph::COMMANDS),
            "{notice:?}"
        );
    }

    /// Esc walks all the way out: a sub page returns to the dashboard, and a
    /// second Esc leaves the harness entirely. The owner found the screen a
    /// dead end, because the only exit was the chord that opened it.
    #[test]
    fn esc_walks_back_out_of_the_harness_from_a_sub_page() {
        use super::super::harness::PageMode;
        let (_dir, mut app, _) = harness_rig();
        app.harness_key(KeyEvent::from(KeyCode::Char('g')));
        assert_eq!(app.harness.as_ref().unwrap().mode, PageMode::Graph);
        app.harness_key(KeyEvent::from(KeyCode::Esc));
        assert_eq!(
            app.harness.as_ref().unwrap().mode,
            PageMode::Dashboard,
            "Esc on a sub page returns to the dashboard"
        );
        app.harness_key(KeyEvent::from(KeyCode::Esc));
        assert!(app.harness.is_none(), "a second Esc must close the harness");
    }

    /// A notice is dismissed before the page closes, so Esc never throws away
    /// the screen when the reader meant to clear a message.
    #[test]
    fn esc_clears_a_notice_before_it_closes_the_page() {
        let (_dir, mut app, _) = harness_rig();
        app.harness_key(KeyEvent::from(KeyCode::Char('l')));
        assert!(
            app.harness.as_ref().unwrap().notice.is_some(),
            "[l] posts a notice"
        );
        app.harness_key(KeyEvent::from(KeyCode::Esc));
        assert!(app.harness.is_some(), "the notice took that Esc");
        assert!(app.harness.as_ref().unwrap().notice.is_none());
        app.harness_key(KeyEvent::from(KeyCode::Esc));
        assert!(app.harness.is_none(), "the next Esc closes");
    }

    // **Two delegation tests stood here and both are gone with the feature
    // they covered.** They built session fixtures out of `queue_wait`,
    // `node_spawn`, `node_done` and `task_progress` records and asserted the
    // Run Graph drew a named node with its own clock, meter, ending and queue
    // depth. Nothing in this tree writes any of those four kinds:
    // `telemetry.rs` is not here, and `harness_state`'s module doc records the
    // grep over 156 real session files on this machine that found none of
    // them. Keeping the tests would mean keeping a reader for records only
    // their own fixtures produce — the false-receipt shape this repository has
    // paid for before. They come back with `telemetry.rs`, in the same change,
    // written against what that module actually appends. See the matching note
    // in `graph_nodes`.

    #[test]
    fn the_open_harness_page_owns_plain_keys_and_cycles_the_selection() {
        let (_dir, mut app, _) = harness_rig();
        assert_eq!(
            app.harness.as_ref().unwrap().runs.len(),
            2,
            "both goals are runs"
        );
        assert_eq!(app.harness.as_ref().unwrap().selected_run, Some(0));
        assert!(
            app.harness_key(KeyEvent::from(KeyCode::Down)),
            "the open page refused a key"
        );
        assert_eq!(app.harness.as_ref().unwrap().selected_run, Some(1));
        let alt_h = KeyEvent::new(KeyCode::Char('h'), KeyModifiers::ALT);
        assert!(
            !app.harness_key(alt_h),
            "Alt+h must stay the shell's toggle"
        );
        assert!(
            app.harness_key(KeyEvent::from(KeyCode::Char('z'))),
            "a plain key escaped the page"
        );
        let mut closed = App::new((161, 50));
        assert!(!closed.harness_key(KeyEvent::from(KeyCode::Char('i'))));
    }

    #[test]
    fn i_mounts_the_inspect_page_from_the_stored_log_and_b_returns() {
        use super::super::harness::PageMode as HMode;
        use super::super::inspect::ToolStatus;
        let (_dir, mut app, _) = harness_rig();
        // Runs list newest first: [1] is the finished goal with the calls.
        app.harness_key(KeyEvent::from(KeyCode::Down));
        app.harness_key(KeyEvent::from(KeyCode::Char('i')));
        let v = app.harness.as_ref().unwrap();
        assert_eq!(v.mode, HMode::Inspect, "[i] did not mount the Inspect page");
        let run = v
            .inspect
            .as_ref()
            .expect("a mounted view")
            .run
            .as_ref()
            .expect("a run");
        assert_eq!(run.name, "Fix the flaky retry test");
        assert_eq!(run.id, "sess-h#1", "the full id, not the display tail");
        assert_eq!(run.status, "Completed");
        assert_eq!(run.tools.len(), 2, "both tool calls are rows");
        assert_eq!(run.tools[0].tool, "Read");
        assert_eq!(run.tools[0].status, ToolStatus::Allowed);
        assert_eq!(
            run.tools[1].status,
            ToolStatus::Denied,
            "a refusal must not read as Allowed"
        );
        assert!(
            run.tools[1].request.contains("src/lib.rs"),
            "args are the request"
        );
        assert!(
            !run.events.is_empty(),
            "the stream is the run's real content"
        );
        // The honest empties: nothing in the log backs these.
        assert!(run.steps.is_empty(), "steps have no backing record");
        assert!(run.artifacts.is_empty(), "artifacts have no backing record");
        assert_eq!(run.eta, "", "an ETA would be an invention");
        assert_eq!(run.branch, "", "no branch record exists");
        assert!(run
            .metadata
            .iter()
            .any(|(l, v)| l == "Session" && v == "sess-h"));
        app.harness_key(KeyEvent::from(KeyCode::Char('b')));
        let v = app.harness.as_ref().unwrap();
        assert_eq!(v.mode, HMode::Dashboard, "[b] did not return");
        assert!(v.inspect.is_none());
    }

    /// The wheel over the Run Graph moves the DAG canvas, and the paint
    /// gives the view the canvas height it clamps against. Every other page
    /// hands the notch back, so the transcript keeps the wheel it had.
    #[test]
    fn the_wheel_scrolls_the_run_graph_and_falls_through_elsewhere() {
        let (_dir, mut app, _) = harness_rig();
        let v = view();
        assert!(
            !app.harness_scroll(false),
            "the dashboard swallowed the wheel"
        );
        // Mount one run's graph, then paint so the canvas reports its height.
        app.harness_key(KeyEvent::from(KeyCode::Char('g')));
        let _ = painted(&mut app, &v, 161, 50);
        let rows = app
            .harness
            .as_ref()
            .unwrap()
            .graph
            .as_ref()
            .unwrap()
            .canvas_rows;
        assert!(rows > 0, "the paint reported no canvas height");
        assert!(
            app.harness_scroll(false),
            "the graph page ignored the wheel"
        );
        let scrolled = app.harness.as_ref().unwrap().graph.as_ref().unwrap().scroll;
        // A graph that fits has nowhere to go; either way the wheel is
        // answered rather than passed down to a transcript nobody can see.
        let max = {
            let g = app.harness.as_ref().unwrap().graph.as_ref().unwrap();
            super::super::rungraph::max_scroll(g)
        };
        assert_eq!(scrolled, max.min(3));
        assert!(app.harness_scroll(true));
        assert_eq!(
            app.harness.as_ref().unwrap().graph.as_ref().unwrap().scroll,
            0
        );
        // While the palette has the keyboard the wheel is not the page's.
        app.harness_key(KeyEvent::from(KeyCode::Char(':')));
        assert!(
            !app.harness_scroll(false),
            "the wheel moved a page someone was typing into"
        );
    }

    #[test]
    fn a_refresh_rereads_the_session_directory() {
        let (dir, mut app, now) = harness_rig();
        let extra = [hgoal(now - 1_000, "A third goal arrives")];
        let lines: Vec<String> = extra.iter().map(|r| r.to_string()).collect();
        std::fs::write(
            dir.path().join("sess-i.jsonl"),
            format!("{}\n", lines.join("\n")),
        )
        .expect("write session");
        app.harness_key(KeyEvent::from(KeyCode::Char('R')));
        assert_eq!(
            app.harness.as_ref().unwrap().runs.len(),
            3,
            "[R] did not re-read"
        );
    }

    #[test]
    fn a_click_selects_a_run_box_and_a_chip_fires_its_key() {
        use super::super::harness::HELP_DASH;
        let (_dir, mut app, _) = harness_rig();
        let v = view();
        let _ = painted(&mut app, &v, 161, 50);
        let hits = app.harness_hits.clone();
        assert!(!hits.runs.is_empty(), "the paint recorded no run boxes");
        let r = hits.runs[1];
        assert!(
            app.harness_click(r.x + 1, r.y + 1),
            "a press on a box fell through"
        );
        assert_eq!(app.harness.as_ref().unwrap().selected_run, Some(1));
        let (chip, _) = hits
            .chips
            .iter()
            .find(|(_, k)| *k == '?')
            .expect("the [?] chip");
        assert!(app.harness_click(chip.x, chip.y));
        assert_eq!(
            app.harness.as_ref().unwrap().notice.as_deref(),
            Some(HELP_DASH),
            "the chip must fire exactly its key's action"
        );
        assert!(!app.harness_click(0, 0), "a miss must fall through");
        app.toggle_harness("/repo");
        assert!(
            !app.harness_click(r.x + 1, r.y + 1),
            "a closed page consumed a click"
        );
    }

    #[test]
    fn the_dashboard_feeds_events_stamps_and_the_current_run_from_the_log() {
        let (_dir, app, _) = harness_rig();
        let v = app.harness.as_ref().unwrap();
        assert!(
            !v.events.is_empty(),
            "the EVENT LOG card starves while the log has records"
        );
        assert!(
            v.events
                .iter()
                .any(|e| e.text.contains("Fix the flaky retry test")
                    || e.text.contains("Investigate the timeout")),
            "the events are not this session's"
        );
        assert!(
            !v.runs[0].stamp.is_empty(),
            "a run's clock stamp is in the log"
        );
        assert_eq!(
            v.current_run, v.runs[0].id,
            "a running run is the current run; the header must say so"
        );
    }

    #[test]
    fn the_adapter_answers_none_for_an_id_no_file_holds() {
        let (dir, _app, now) = harness_rig();
        assert!(inspect_view_from(dir.path(), "sess-h#9", "v0", now).is_none());
    }

    // -- the Settings screen, live (settings-live) ---------------------------

    fn open_settings_at(home: &std::path::Path) -> App {
        let mut app = App::new((161, 75));
        app.set_home(home.to_path_buf());
        app.toggle_settings();
        assert!(app.settings_open());
        app
    }

    fn skey(app: &mut App, code: KeyCode) -> bool {
        app.settings_key(KeyEvent::from(code))
    }

    /// Paint once so the control rects exist for a click test.
    fn paint_settings(app: &mut App) {
        let area = Rect::new(0, 0, 161, 75);
        let mut buf = Buffer::empty(area);
        app.render(area, &mut buf, &view(), &settings_bar());
    }

    /// Class A: the Theme row cycles the names this run can actually select
    /// and persists exactly as /theme does — the name lands in this home's
    /// settings.json and the receipt names the file.
    ///
    /// **The list is a directory, not a table**, so the fixture writes a second
    /// theme file to have something to cycle *to*. On the tree this test came
    /// from `THEMES` was a compiled-in array and `THEMES[1]` was always there;
    /// here a machine with no theme files can only select the built-in, and a
    /// cycler over one name that asserted it moved would be asserting a bug.
    #[test]
    fn the_theme_row_cycles_the_registry_and_persists_like_slash_theme() {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join(".emma").join("themes");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("oxide.json"), "{}").unwrap();
        let mut app = open_settings_at(home.path());
        assert_eq!(
            app.settings.themes,
            vec!["emma".to_string(), "oxide".to_string()],
            "the built-in first, then the file"
        );
        app.settings.theme = "emma".to_string();
        app.settings.focus = Some((2, 0));
        assert!(skey(&mut app, KeyCode::Right));
        let stored = crate::settings::load(home.path());
        assert_eq!(
            stored.theme.as_deref(),
            Some("oxide"),
            "the cycle must persist through the /theme mechanism"
        );
        assert_eq!(app.settings.theme, "oxide");
        let notice = app.settings.notice.clone().unwrap();
        assert!(notice.contains("settings.json"), "no receipt: {notice}");
        assert!(
            notice.contains("next start"),
            "a theme is read once, at startup, and the row must say so: {notice}"
        );
    }

    /// Class A: Enable Memory keeps the additive default-on semantics — Off
    /// writes `false`, On removes the key rather than writing `true`.
    #[test]
    fn the_memory_toggle_writes_false_and_removes_the_key() {
        let home = tempfile::tempdir().unwrap();
        let mut app = open_settings_at(home.path());
        assert!(app.settings.memory_on, "absent means on");
        app.settings.focus = Some((4, 0));
        assert!(skey(&mut app, KeyCode::Enter));
        assert_eq!(crate::settings::load(home.path()).memory, Some(false));
        assert!(!app.settings.memory_on);
        assert!(skey(&mut app, KeyCode::Enter));
        let stored = crate::settings::load(home.path());
        assert_eq!(stored.memory, None, "On must remove the key");
        assert!(stored.memory.unwrap_or(true));
        assert!(app.settings.memory_on);
    }

    /// Class B: opening the screen reports every language in the table,
    /// against what `emma_tools_lsp` actually finds on disk.
    ///
    /// `found` is deliberately not asserted: whether any given server is on
    /// the machine running the test is a fact about that machine. What is
    /// asserted is the table, the default enabled set, and the network flags,
    /// which are facts about this build.
    #[test]
    fn opening_settings_reports_every_language_in_the_table() {
        let home = tempfile::tempdir().unwrap();
        let app = open_settings_at(home.path());
        let keys: Vec<&str> = app.settings.lsp.iter().map(|r| r.key.as_str()).collect();
        assert_eq!(keys, emma_tools_lsp::lang::keys());
        for row in &app.settings.lsp {
            let l = emma_tools_lsp::lang::by_key(&row.key).expect("in the table");
            assert_eq!(row.network, l.network, "{}", row.key);
            assert_eq!(
                row.enabled,
                emma_tools_lsp::lang::DEFAULT_ENABLED.contains(&l.key),
                "with no lsp.enabled written, the row must follow the default set: {}",
                row.key
            );
        }
        assert!(app.settings.lsp_unknown.is_empty());
    }

    /// Class A: Save Now writes the file and the receipt names it.
    #[test]
    fn save_now_writes_settings_json_with_a_receipt() {
        let home = tempfile::tempdir().unwrap();
        let mut app = open_settings_at(home.path());
        app.settings.focus = Some((7, 0));
        assert!(skey(&mut app, KeyCode::Enter));
        assert!(crate::settings::path(home.path()).exists());
        let notice = app.settings.notice.clone().unwrap();
        assert!(notice.contains("settings.json"), "no receipt: {notice}");
    }

    /// Class A: Export writes a timestamped byte-for-byte copy beside
    /// settings.json and the receipt names the path.
    #[test]
    fn export_writes_a_timestamped_copy_and_names_it() {
        let home = tempfile::tempdir().unwrap();
        let mut stored = crate::settings::load(home.path());
        stored.theme = Some("mono".to_string());
        crate::settings::save(home.path(), &stored).unwrap();
        let mut app = open_settings_at(home.path());
        app.settings.focus = Some((7, 1));
        assert!(skey(&mut app, KeyCode::Enter));
        let dir = crate::settings::path(home.path());
        let dir = dir.parent().unwrap();
        let copy = std::fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .find(|e| {
                let n = e.file_name().to_string_lossy().to_string();
                n.starts_with("settings-") && n.ends_with(".json")
            })
            .expect("a timestamped copy beside settings.json");
        assert_eq!(
            std::fs::read(copy.path()).unwrap(),
            std::fs::read(crate::settings::path(home.path())).unwrap(),
            "the copy must be byte-for-byte"
        );
        let notice = app.settings.notice.clone().unwrap();
        assert!(
            notice.contains(&copy.file_name().to_string_lossy().to_string()),
            "the receipt must name the copy: {notice}"
        );
    }

    /// Reset asks first, fires second, and clears only the additive keys the
    /// screen owns — the provider and its models are another surface's.
    #[test]
    fn reset_confirms_then_clears_only_the_keys_it_owns() {
        let home = tempfile::tempdir().unwrap();
        let mut stored = crate::settings::load(home.path());
        stored.provider = Some("ollama".to_string());
        stored
            .models
            .insert("ollama".to_string(), "llama3:8b".to_string());
        stored.theme = Some("dracula".to_string());
        stored.memory = Some(false);
        crate::settings::save(home.path(), &stored).unwrap();
        let mut app = open_settings_at(home.path());
        app.settings.focus = Some((7, 2));
        assert!(skey(&mut app, KeyCode::Enter));
        assert_eq!(
            crate::settings::load(home.path()).theme.as_deref(),
            Some("dracula"),
            "the first Enter must not touch the file"
        );
        assert!(app.settings.confirm_reset);
        assert!(skey(&mut app, KeyCode::Enter));
        let after = crate::settings::load(home.path());
        assert_eq!(after.theme, None, "Reset owns theme");
        assert_eq!(after.memory, None, "Reset owns memory_capture");
        assert_eq!(
            after.provider.as_deref(),
            Some("ollama"),
            "Reset must not touch the provider"
        );
        assert_eq!(
            after.models.get("ollama").map(String::as_str),
            Some("llama3:8b"),
            "Reset must not touch the models"
        );
        let notice = app.settings.notice.clone().unwrap();
        assert!(notice.contains("theme"), "the receipt says what it cleared");
        assert!(
            notice.contains("memory"),
            "the receipt says what it cleared"
        );
    }

    /// A click is its key: the theme cycler's right chevron lands the same
    /// disk write as focusing the row and pressing →, and a static row's
    /// value answers with the same notice Enter gives.
    #[test]
    fn a_click_is_its_key() {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join(".emma").join("themes");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("oxide.json"), "{}").unwrap();
        let mut app = open_settings_at(home.path());
        app.settings.theme = "emma".to_string();
        paint_settings(&mut app);
        let next = app
            .settings_hits
            .controls
            .iter()
            .find(|(_, h)| *h == super::super::settings::Hit::Next(2, 0))
            .map(|(r, _)| *r)
            .expect("the theme chevron was painted");
        assert!(app.settings_click(next.x, next.y));
        assert_eq!(
            crate::settings::load(home.path()).theme.as_deref(),
            Some("oxide"),
            "the click must land the key's own write"
        );
        assert_eq!(app.settings.focus, Some((2, 0)), "the click focuses too");
        // A static row answers with the honest notice. Font Family rather
        // than a permission row: card 6's rows are a function of whichever
        // project the test binary happens to be run from, and a test that is
        // silently a fact about the developer's checkout is the shape
        // `guarantees.rs` refuses for the Harness page.
        paint_settings(&mut app);
        let font = app
            .settings_hits
            .controls
            .iter()
            .find(|(_, h)| *h == super::super::settings::Hit::Act(2, 2))
            .map(|(r, _)| *r)
            .expect("the Font Family value was painted");
        assert!(app.settings_click(font.x, font.y));
        assert_eq!(
            app.settings.notice.as_deref(),
            Some(super::super::settings::NOTICE_FONT)
        );
        // Empty ground falls through — the sidebar's rule.
        assert!(!app.settings_click(0, 0));
    }

    /// Test Connection is real against a local Ollama: a listener answers
    /// [ OK ], a dead port answers [ FAIL ], and a hosted provider gets the
    /// honest notice instead of a fake green.
    #[test]
    fn test_connection_reports_reality() {
        use super::super::settings::TestState;
        let home = tempfile::tempdir().unwrap();
        let mut app = open_settings_at(home.path());
        app.settings.provider = "ollama".to_string();
        app.settings.focus = Some((0, 5));
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let alive = listener.local_addr().unwrap();
        app.set_ollama_host(format!("127.0.0.1:{}", alive.port()));
        assert!(skey(&mut app, KeyCode::Enter));
        assert_eq!(app.settings.test, TestState::Ok);
        // A port nothing listens on: bind, learn it, free it.
        let dead = std::net::TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap();
        app.set_ollama_host(format!("127.0.0.1:{}", dead.port()));
        assert!(skey(&mut app, KeyCode::Enter));
        assert_eq!(app.settings.test, TestState::Fail);
        drop(listener);
        // Hosted: the truth, not a ping that could hang the input thread.
        app.settings.provider = "anthropic".to_string();
        app.settings.test = TestState::Idle;
        assert!(skey(&mut app, KeyCode::Enter));
        assert_eq!(app.settings.test, TestState::Idle);
        let notice = app.settings.notice.clone().unwrap();
        assert!(notice.contains("hosted"), "{notice}");
    }

    /// Esc through the shell seam: with focus it clears, without it the
    /// screen closes — and the page consumes every plain key while open.
    #[test]
    fn esc_clears_then_closes_through_the_shell_seam() {
        let home = tempfile::tempdir().unwrap();
        let mut app = open_settings_at(home.path());
        assert!(skey(&mut app, KeyCode::Tab));
        assert!(skey(&mut app, KeyCode::Esc));
        assert!(app.settings_open(), "the first Esc only clears focus");
        assert!(skey(&mut app, KeyCode::Esc));
        assert!(!app.settings_open(), "the second Esc closes");
        assert!(
            !skey(&mut app, KeyCode::Esc),
            "a closed screen consumes nothing"
        );
    }
}
