//! One table per surface: the chord, what it does, and the proof that
//! something answers it.
//!
//! # Why this module exists at all
//!
//! The fork inventory counted a `QUICK HELP` panel
//! advertising six keys of which four do nothing, on a branch about to be
//! merged here. Nobody wrote those rows dishonestly. The table was a literal
//! list of `(key, description)` pairs and the bindings were an exhaustive
//! `match` in another file, and a literal list agrees with its author on the
//! day it is written and drifts from then on. The owner has already lost an
//! evening to the same shape in the live tree — the `Alt` chords — and ruled:
//! *"I don't want to get into the app and notice an interaction button is not
//! working."*
//!
//! A test that compares the list against the handlers does not fix this. If
//! the list is hand-written **and** the drawing is separate, that test proves
//! the list matches the handlers and says nothing about what is on screen —
//! the same defect wearing a receipt. So the rule here is the one
//! The coverage contract asks for:
//!
//! > **The declaration is the thing that draws.** One value, read twice —
//! > once to paint, once to test.
//!
//! [`Table::hints`] is what the sidebar's `QUICK HELP` panel and the Settings
//! page's `Keys` block render; [`resolve`] is what the reader loop's four
//! decoders say about the same chord. A row cannot be painted without a
//! [`Chord`] beside it, and [`Table::hints`] derives the printed label *from*
//! that chord rather than taking a second string — so a row whose key nothing
//! answers is not a row somebody has to notice, it is a row the tests below
//! turn red.
//!
//! # What is enforced by the type system and what is enforced by a test
//!
//! Being precise about this, because "unrepresentable" is a strong word and
//! only half of it is earned today.
//!
//! - **Enforced by construction:** the printed label. There is no field for
//!   it. `Ctrl+B` on screen is computed from `Chord { Char('b'), CONTROL }`,
//!   so the panel cannot spell a key differently from the key it means, and
//!   cannot name a key that is not in the table.
//! - **Enforced by a test:** liveness. Rust cannot ask a `match` which arms
//!   it has, so `every_drawn_key_is_answered` drives each chord
//!   through the real decoders and fails on anything that comes back
//!   [`Action::Ignore`]. Deleting the `Ctrl-B` arm from
//!   [`super::input::pane_key`] goes red there, not at compile time.
//!
//! # The other direction, which is the one people skip
//!
//! Every *handled* key must be drawn, or deliberately undrawn with the reason
//! written down. [`UNDRAWN`] is that record, and it is not a list that can rot
//! quietly: `every_handled_key_is_drawn_or_deliberately_hidden`
//! probes several thousand keystrokes through [`resolve`] and fails on any
//! that does something and appears in neither table, while
//! `nothing_hidden_is_already_dead` fails on an [`UNDRAWN`] row whose
//! key stopped working. Both directions are pinned, so the pair drifts loudly.
//!
//! # What this module is not
//!
//! [`resolve`] is a **second statement of the reader loop's precedence**, not
//! the loop itself. It calls the same four decoders in the same order
//! `LineSource::raw` calls them, so any change to what a key *does* is picked
//! up here; only a change to the *order* could drift, and that order is four
//! branches. The honest fix is for the loop to call this function — see the
//! report; `input.rs` is shared and was not this change's to edit.
//!
//! And a cell buffer is not a console. Nothing here presses a key on a real
//! terminal: [`Trigger::Terminal`] rows in particular are claims about what
//! Emma refuses to claim, and only a human at a real console can confirm the
//! terminal picks them up.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use super::input::{menu_key, pane_key, tool_key, Action, Editor, MenuKey, PaneKey};

// region: The vocabulary
// ---------------------------------------------------------------------------
// The vocabulary
//
// A chord, what a chord is expected to reach, and the state of the world the
// expectation holds in. All three are `const`-constructible so a table can be
// a `static` — which is what stops a page building its hints from one value
// and its dispatch from another.
// ---------------------------------------------------------------------------

/// A keystroke, as the decoders see one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Chord {
    pub code: KeyCode,
    pub mods: KeyModifiers,
}

impl Chord {
    pub const fn plain(code: KeyCode) -> Self {
        Self {
            code,
            mods: KeyModifiers::NONE,
        }
    }

    pub const fn ctrl(c: char) -> Self {
        Self {
            code: KeyCode::Char(c),
            mods: KeyModifiers::CONTROL,
        }
    }

    pub const fn ctrl_code(code: KeyCode) -> Self {
        Self {
            code,
            mods: KeyModifiers::CONTROL,
        }
    }

    /// The crossterm event this chord stands for, on the press edge — the only
    /// edge anything in Emma acts on (Windows reports releases too, and acting
    /// on both types every character twice).
    pub fn press(self) -> KeyEvent {
        KeyEvent::new(self.code, self.mods)
    }

    /// How the chord is spelled on screen.
    ///
    /// **Derived, never stored.** This is the half of the `QUICK HELP` defect
    /// that a type can carry: with no field to type a label into, the panel
    /// cannot advertise `Ctrl+k` for a binding that is `Ctrl-U`.
    ///
    /// The spellings are the ones already on screen in `app.rs`'s `keymap`,
    /// because changing what the panel says was not this change's business —
    /// `PgUp`/`PgDn` rather than `PageUp`, `Dn` rather than `Down`, a bare `/`
    /// rather than `Slash`, and letters upper-cased because `Ctrl+b` reads as
    /// a shift-sensitive binding and is not one.
    pub fn label(self) -> String {
        format!("{}{}", mods_prefix(self.mods), key_name(self.code))
    }
}

fn mods_prefix(mods: KeyModifiers) -> String {
    let mut out = String::new();
    if mods.contains(KeyModifiers::CONTROL) {
        out.push_str("Ctrl+");
    }
    if mods.contains(KeyModifiers::ALT) {
        out.push_str("Alt+");
    }
    if mods.contains(KeyModifiers::SHIFT) {
        out.push_str("Shift+");
    }
    out
}

fn key_name(code: KeyCode) -> String {
    match code {
        KeyCode::Char('/') => "/".to_string(),
        KeyCode::Char(c) => c.to_ascii_uppercase().to_string(),
        KeyCode::Enter => "Enter".to_string(),
        KeyCode::Esc => "Esc".to_string(),
        KeyCode::Tab => "Tab".to_string(),
        KeyCode::BackTab => "Shift+Tab".to_string(),
        KeyCode::Backspace => "Backspace".to_string(),
        KeyCode::Delete => "Delete".to_string(),
        KeyCode::Insert => "Insert".to_string(),
        KeyCode::Left => "Left".to_string(),
        KeyCode::Right => "Right".to_string(),
        KeyCode::Up => "Up".to_string(),
        KeyCode::Down => "Dn".to_string(),
        KeyCode::Home => "Home".to_string(),
        KeyCode::End => "End".to_string(),
        KeyCode::PageUp => "PgUp".to_string(),
        KeyCode::PageDown => "PgDn".to_string(),
        KeyCode::F(n) => format!("F{n}"),
        other => format!("{other:?}"),
    }
}

/// What a chord is claimed to reach.
///
/// The claim is coarse on purpose: it names the decoder and the branch, not
/// the effect. "`Ctrl-B` reaches [`PaneKey::Sidebar`]" is checkable here;
/// "`Ctrl-B` toggles the sidebar" is `app.rs`'s to prove, and it already does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Expect {
    Pane(PaneKey),
    Menu(MenuKey),
    /// `Alt+<letter>` reaches the tool catalogue. Which letter is bound is the
    /// catalogue's knowledge, not this table's.
    Tool,
    /// Esc, with a page up and the menu shut.
    LeavePage,
    Submit,
    Interrupt,
    Eof,
    /// Reaches the line editor and changes it — `/` is the case that matters,
    /// because typing it is what opens the command menu.
    Edits,
    /// Nothing in Emma answers it, and that is the point: the terminal does.
    /// Only a human at a real console can confirm one of these.
    Terminal,
}

/// The state of the world a binding's claim holds in.
///
/// A binding that is live *only* under some condition has to say which, or the
/// test either passes vacuously or fails for the wrong reason: `Home`/`End`
/// reach the transcript on an empty box and the cursor otherwise, and that is
/// the binding's meaning rather than an exception to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ctx {
    pub editor_empty: bool,
    pub prompt_pending: bool,
    pub menu_open: bool,
    pub page_showing: bool,
}

impl Ctx {
    /// An idle chat pane: nothing typed, no question, no menu, no page. The
    /// state the sidebar's panel is read in.
    pub const IDLE: Ctx = Ctx {
        editor_empty: true,
        prompt_pending: false,
        menu_open: false,
        page_showing: false,
    };

    pub const fn with_menu(self) -> Ctx {
        Ctx {
            menu_open: true,
            ..self
        }
    }

    pub const fn with_page(self) -> Ctx {
        Ctx {
            page_showing: true,
            ..self
        }
    }

    pub const fn typing(self) -> Ctx {
        Ctx {
            editor_empty: false,
            ..self
        }
    }
}

/// What a row is triggered by.
#[derive(Debug, Clone, Copy)]
pub enum Trigger {
    /// One or more chords sharing a description, each with its own claim.
    /// Several because the panel says `PgUp/PgDn` on one row, and splitting
    /// that into two rows would be a change to the screen rather than to its
    /// honesty.
    Keys(&'static [(Chord, Expect)]),
    /// Resolves from the active keymap at [`Binding::label`] time, so a
    /// rebound chord cannot leave a stale spelling on the QUICK HELP panel.
    Bound(super::keymap::Action),
    /// `Alt+<any letter>`: the whole family, because the letters belong to the
    /// user-tool catalogue and change with it.
    AltAny,
    /// Not a keystroke Emma decodes. The label is given here because there is
    /// no `Chord` to derive one from — a mouse gesture has no `KeyCode`.
    Terminal(&'static str),
}

/// One row: what draws, and what answers.
#[derive(Debug, Clone, Copy)]
pub struct Binding {
    pub trigger: Trigger,
    /// The right-hand column of the panel.
    pub what: &'static str,
    /// The world the claim holds in.
    pub ctx: Ctx,
}

impl Binding {
    /// The left-hand column: derived from the chords, joined the way the panel
    /// already joins them — a shared modifier is factored out, so two chords
    /// reading `Ctrl+Up` and `Ctrl+Dn` print as `Ctrl+Up/Dn` rather than
    /// repeating the prefix.
    pub fn label(&self) -> String {
        match self.trigger {
            Trigger::Terminal(text) => text.to_string(),
            Trigger::AltAny => "Alt+key".to_string(),
            Trigger::Bound(action) => super::keymap::active()
                .chord_for(action)
                .and_then(|s| super::keymap::parse_chord(&s))
                .map(|kc| {
                    let mut mods = KeyModifiers::NONE;
                    if kc.ctrl {
                        mods |= KeyModifiers::CONTROL;
                    }
                    if kc.alt {
                        mods |= KeyModifiers::ALT;
                    }
                    Chord {
                        code: KeyCode::Char(kc.key),
                        mods,
                    }
                    .label()
                })
                .unwrap_or_else(|| "?".to_string()),
            Trigger::Keys(keys) => {
                let shared = keys
                    .first()
                    .map(|(c, _)| c.mods)
                    .filter(|m| keys.iter().all(|(c, _)| c.mods == *m))
                    .unwrap_or(KeyModifiers::NONE);
                let names: Vec<String> = keys.iter().map(|(c, _)| key_name(c.code)).collect();
                format!("{}{}", mods_prefix(shared), names.join("/"))
            }
        }
    }
}

/// A surface's whole key vocabulary.
pub struct Table {
    /// What the surface draws, in the order it draws it.
    pub drawn: &'static [Binding],
}

impl Table {
    /// The `(key, description)` rows the panel renders.
    ///
    /// This is the value `app.rs`'s `keymap` returns, and the sidebar's
    /// `QUICK HELP` and the Settings page's `Keys` block both read it. It is
    /// deliberately the *only* way to get rows out of a table: a caller that
    /// wanted to add a twelfth row would have to add a `Binding`, and a
    /// `Binding` carries a `Chord` the tests below will drive.
    pub fn hints(&self) -> Vec<(String, String)> {
        self.drawn
            .iter()
            .map(|b| (b.label(), b.what.to_string()))
            .collect()
    }
}

// endregion: The vocabulary

// region: The chat surface
// ---------------------------------------------------------------------------
// The chat surface
//
// Eleven rows, and every one of them is a key the live tree actually decodes.
// This is the table the fork's §11 finding is about, transplanted onto the
// live tree's real bindings and given a chord per row so the claim is
// checkable.
// ---------------------------------------------------------------------------

const SLASH: &[(Chord, Expect)] = &[(Chord::plain(KeyCode::Char('/')), Expect::Edits)];
const ENTER: &[(Chord, Expect)] = &[(Chord::plain(KeyCode::Enter), Expect::Submit)];
const ESC: &[(Chord, Expect)] = &[(Chord::plain(KeyCode::Esc), Expect::LeavePage)];
const PAGES: &[(Chord, Expect)] = &[
    (Chord::plain(KeyCode::PageUp), Expect::Pane(PaneKey::PageUp)),
    (
        Chord::plain(KeyCode::PageDown),
        Expect::Pane(PaneKey::PageDown),
    ),
];
const ROWS: &[(Chord, Expect)] = &[
    (Chord::ctrl_code(KeyCode::Up), Expect::Pane(PaneKey::RowUp)),
    (
        Chord::ctrl_code(KeyCode::Down),
        Expect::Pane(PaneKey::RowDown),
    ),
];
const ENDS: &[(Chord, Expect)] = &[
    (Chord::plain(KeyCode::Home), Expect::Pane(PaneKey::Top)),
    (Chord::plain(KeyCode::End), Expect::Pane(PaneKey::Tail)),
];
const INTERRUPT: &[(Chord, Expect)] = &[(Chord::ctrl('c'), Expect::Interrupt)];
const QUIT: &[(Chord, Expect)] = &[(Chord::ctrl('d'), Expect::Eof)];

/// Alt+q, and it is a row of its own rather than a second chord on [`QUIT`].
///
/// `Binding::label` factors out a shared modifier, and `Ctrl+D` and `Alt+q`
/// share none, so one row holding both would print `D/q` and name neither.
/// It is drawn at all because `input::interrupt_notice` puts the chord in a
/// line Emma prints, and a key named in Emma's own output that is absent from
/// the panel claiming to list the keys is the drawn-control problem read
/// backwards.
const ALT_QUIT: &[(Chord, Expect)] = &[(
    Chord {
        code: KeyCode::Char('q'),
        mods: KeyModifiers::ALT,
    },
    Expect::Pane(PaneKey::Quit),
)];

/// The chat pane's advertised keys — the `QUICK HELP` table, and the Settings
/// page's copy of it.
///
/// Order is the order they are drawn in, and it is the order they were already
/// drawn in: the panel is something a user scans, and reordering it to suit an
/// enumeration would be the tail wagging the dog.
pub static CHAT: Table = Table {
    drawn: &[
        Binding {
            trigger: Trigger::Keys(SLASH),
            what: "command menu",
            ctx: Ctx::IDLE,
        },
        Binding {
            trigger: Trigger::Keys(ENTER),
            what: "send",
            // Typing, because `Enter` on an empty box submits an empty line —
            // true, but not what the row is about.
            ctx: Ctx::IDLE.typing(),
        },
        Binding {
            trigger: Trigger::Keys(ESC),
            what: "close menu / leave page",
            // The row makes two claims. This one is the page half; the menu
            // half is asserted separately in `esc_dismisses_the_menu_before_it_
            // leaves_a_page`, because a `Ctx` cannot hold both at once and the
            // ordering between them is the interesting part.
            ctx: Ctx::IDLE.with_page(),
        },
        Binding {
            trigger: Trigger::Keys(PAGES),
            what: "scroll",
            ctx: Ctx::IDLE,
        },
        Binding {
            trigger: Trigger::Keys(ROWS),
            what: "scroll a row",
            ctx: Ctx::IDLE,
        },
        Binding {
            trigger: Trigger::Keys(ENDS),
            what: "top / tail (empty box)",
            ctx: Ctx::IDLE,
        },
        Binding {
            trigger: Trigger::Bound(super::keymap::Action::Sidebar),
            what: "toggle sidebar",
            ctx: Ctx::IDLE,
        },
        Binding {
            trigger: Trigger::Bound(super::keymap::Action::Help),
            what: "help",
            ctx: Ctx::IDLE,
        },
        Binding {
            trigger: Trigger::AltAny,
            what: "tool or page",
            ctx: Ctx::IDLE,
        },
        Binding {
            trigger: Trigger::Keys(INTERRUPT),
            what: "interrupt",
            ctx: Ctx::IDLE,
        },
        Binding {
            trigger: Trigger::Keys(QUIT),
            what: "quit",
            ctx: Ctx::IDLE,
        },
        Binding {
            trigger: Trigger::Keys(ALT_QUIT),
            what: "quit, or interrupt a running goal",
            ctx: Ctx::IDLE,
        },
        Binding {
            trigger: Trigger::Terminal("Shift+drag"),
            what: "select text",
            ctx: Ctx::IDLE,
        },
    ],
};

/// Keys the live tree decodes and draws nowhere, each with why.
///
/// **This is the half of the contract people skip, and it is the half the
/// fork's §11 finding ends on**: the six real editor bindings appear on no
/// rendered surface. Listing them is not an excuse for that — several of these
/// reasons are thin, and the ones that are say so. It is the difference
/// between a gap somebody decided and a gap nobody noticed.
///
/// The list cannot rot quietly in either direction. A new handled key that is
/// not here fails `every_handled_key_is_drawn_or_deliberately_hidden`; a row
/// here whose key stopped working fails `nothing_hidden_is_already_dead`.
pub static UNDRAWN: &[(Chord, Ctx, &str)] = &[
    (
        Chord::plain(KeyCode::Char('?')),
        Ctx::IDLE,
 "open the Help page from an empty box. The drawn chord is Ctrl+/, which works with text in the box too; `?` is the fork's second door for people who reach for it, and the Help page's own text names both.",
    ),
    (
        Chord::plain(KeyCode::Char('?')),
        Ctx::IDLE.with_page(),
        "the same key over an open page, where Ctrl+/ also answers.",
    ),
    (
        Chord::ctrl('7'),
        Ctx::IDLE,
 "Ctrl+/ as some terminals report it. The 0x1F byte a terminal sends for Ctrl+/ reaches crossterm as `/`, `_` or `7` depending on keyboard and terminal, and `pane_key` folds all three to `/` before any lookup, so this is the drawn Ctrl+/ under another name rather than a key.",
    ),
    (
        Chord::ctrl('_'),
        Ctx::IDLE,
        "Ctrl+/ as other terminals report it; see Ctrl+7.",
    ),
    (
        Chord::ctrl('u'),
        Ctx::IDLE.typing(),
        "clear the line. A real binding on no rendered surface — the same gap \
         the fork's inventory §11 ends on, recorded here rather than fixed, \
         because widening the panel is a change to the screen and wants the \
         owner's eye. `Ctrl+k` in the fork's panel is this binding \
         misremembered.",
    ),
    (
        Chord::ctrl('w'),
        Ctx::IDLE.typing(),
        "delete the word behind the cursor. Undrawn for the same reason as \
         Ctrl-U, and with the better excuse that a reader who knows Ctrl-W \
         knows it from every other readline box.",
    ),
    (
        Chord::ctrl_code(KeyCode::Left),
        Ctx::IDLE.typing(),
        "cursor back one word. Added 2026-08-27, after the sweep below showed \
         `Ctrl-Left` falling through to the plain `Left` arm and moving one \
         column — the narrow thing, quietly, where every other editor moves a \
         word. Undrawn because it is the most universal chord in the box and \
         four more rows would crowd a panel the owner has not asked to widen.",
    ),
    (
        Chord::ctrl_code(KeyCode::Right),
        Ctx::IDLE.typing(),
        "cursor forward one word. The mirror of Ctrl-Left; same reason.",
    ),
    (
        Chord::ctrl_code(KeyCode::Backspace),
        Ctx::IDLE.typing(),
        "delete the word behind the cursor. The same edit as Ctrl-W, under the \
         spelling most people reach for first; both share `Editor::word_start` \
         so there is one definition of where a word begins.",
    ),
    (
        Chord::ctrl_code(KeyCode::Delete),
        Ctx::IDLE.typing(),
        "delete the word ahead of the cursor. The forward mirror, and the one \
         of the four with no readline equivalent to fall back on.",
    ),
    (
        Chord::ctrl('a'),
        Ctx::IDLE.typing(),
        "cursor to the start. Readline muscle memory; undrawn.",
    ),
    (
        Chord::ctrl('e'),
        Ctx::IDLE.typing(),
        "cursor to the end. Readline muscle memory; undrawn.",
    ),
    (
        Chord::plain(KeyCode::Tab),
        Ctx::IDLE.with_menu(),
        "accept the highlighted command, and only while the menu is open. \
         Undrawn because the menu itself is the affordance — it is on screen \
         when the key is live, with the selection highlighted. Advertising \
         Tab in a panel that is visible when the menu is *shut* would say it \
         completes something, which is what the fork's panel says and is \
         false there too.",
    ),
    (
        Chord::plain(KeyCode::Up),
        Ctx::IDLE.with_menu(),
        "move the menu selection up. Undrawn: an arrow key over a highlighted \
         list needs no caption.",
    ),
    (
        Chord::plain(KeyCode::Down),
        Ctx::IDLE.with_menu(),
        "move the menu selection down. Same reason as Up.",
    ),
];

// endregion: The chat surface

// region: Resolution
// ---------------------------------------------------------------------------
// Resolution
//
// The reader loop's precedence, said once, purely. `LineSource::raw` decides
// the same thing inline over a `Frame`, which needs a real terminal; this is
// the part with the decision in it, where a test can reach it.
// ---------------------------------------------------------------------------

/// Where a keystroke ends up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    Pane(PaneKey),
    Tool(char),
    LeavePage,
    Menu(MenuKey),
    Edit(Action),
}

impl Answer {
    /// Whether anything at all happened. `Edit(Ignore)` is the shape a dead
    /// key has: the editor's catch-all arm, which swallows every modified key
    /// it does not implement so `Ctrl-R` does not type an `r`.
    pub fn is_live(&self) -> bool {
        !matches!(self, Answer::Edit(Action::Ignore))
    }
}

/// Run one keystroke through the four decoders, in the order
/// `LineSource::raw` runs them.
///
/// The editor is taken by reference and mutated, because `Submit`, `Eof` and
/// the motion keys are answers *about the editor's state* — a version that
/// took a fresh editor every time would report `Ctrl-D` as `Eof` whatever was
/// typed, which is the opposite of what that binding guards.
pub fn resolve(editor: &mut Editor, key: KeyEvent, ctx: Ctx) -> Answer {
    if let Some(cmd) = pane_key(key, ctx.editor_empty, ctx.prompt_pending) {
        return Answer::Pane(cmd);
    }
    if let Some(c) = tool_key(key, ctx.prompt_pending) {
        return Answer::Tool(c);
    }
    if key.kind != KeyEventKind::Release
        && key.code == KeyCode::Esc
        && !ctx.prompt_pending
        && !ctx.menu_open
        && ctx.page_showing
    {
        return Answer::LeavePage;
    }
    if let Some(owned) = menu_key(key, ctx.menu_open) {
        return Answer::Menu(owned);
    }
    Answer::Edit(editor.key(key))
}

// endregion: Resolution

#[cfg(test)]
mod tests {
    use super::*;
    use crate::term::keymap::{self, Action as KeyAction};
    use crate::term::menu::Menu;

    /// The keymap is process-wide state; serialise around tests that install one.
    /// Callers hold `keymap::test_lock()` already; the lock is not reentrant.
    fn restore_keymap() {
        keymap::install(keymap::Keymap::compiled());
    }

    fn with_keymap(json: &str, f: impl FnOnce()) {
        let _lock = keymap::test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        keymap::install(keymap::parse(json));
        f();
        // Not `restore_keymap()`: the lock above is held and is not reentrant.
        keymap::install(keymap::Keymap::compiled());
    }

    fn chord_for_action(action: KeyAction) -> Chord {
        let spell = keymap::active()
            .chord_for(action)
            .expect("action has a chord");
        let kc = keymap::parse_chord(&spell).expect("parses");
        let mut mods = KeyModifiers::NONE;
        if kc.ctrl {
            mods |= KeyModifiers::CONTROL;
        }
        if kc.alt {
            mods |= KeyModifiers::ALT;
        }
        Chord {
            code: KeyCode::Char(kc.key),
            mods,
        }
    }

    /// Resolve a chord against a freshly-built editor whose emptiness matches the
    /// context, which is what nearly every check below wants.
    fn answer(chord: Chord, ctx: Ctx) -> Answer {
        let mut editor = Editor::default();
        if !ctx.editor_empty {
            editor.paste("goal");
        }
        resolve(&mut editor, chord.press(), ctx)
    }

    /// Whether an answer matches the claim the table makes for it.
    fn matches(answer: &Answer, expect: Expect) -> bool {
        match (answer, expect) {
            (Answer::Pane(got), Expect::Pane(want)) => *got == want,
            (Answer::Menu(got), Expect::Menu(want)) => *got == want,
            (Answer::Tool(_), Expect::Tool) => true,
            (Answer::LeavePage, Expect::LeavePage) => true,
            (Answer::Edit(Action::Submit(_)), Expect::Submit) => true,
            (Answer::Edit(Action::Interrupt), Expect::Interrupt) => true,
            (Answer::Edit(Action::Eof), Expect::Eof) => true,
            (Answer::Edit(Action::Edit), Expect::Edits) => true,
            _ => false,
        }
    }

    /// Every keystroke worth probing: the printable range a keyboard can send,
    /// plus every named key any of the four decoders mentions, crossed with
    /// the modifier sets a terminal reports.
    ///
    /// **Enumerated rather than listed.** The point of the reverse direction is
    /// to find a binding nobody wrote down, so a hand-picked candidate set
    /// would only ever find the ones somebody remembered — which is the defect,
    /// one level up.
    fn every_keystroke() -> Vec<KeyEvent> {
        let mut codes: Vec<KeyCode> = (0x20u8..0x7f).map(|b| KeyCode::Char(b as char)).collect();
        codes.extend([
            KeyCode::Enter,
            KeyCode::Esc,
            KeyCode::Tab,
            KeyCode::BackTab,
            KeyCode::Backspace,
            KeyCode::Delete,
            KeyCode::Insert,
            KeyCode::Left,
            KeyCode::Right,
            KeyCode::Up,
            KeyCode::Down,
            KeyCode::Home,
            KeyCode::End,
            KeyCode::PageUp,
            KeyCode::PageDown,
        ]);
        codes.extend((1u8..=12).map(KeyCode::F));
        let mods = [
            KeyModifiers::NONE,
            KeyModifiers::SHIFT,
            KeyModifiers::CONTROL,
            KeyModifiers::ALT,
            KeyModifiers::CONTROL | KeyModifiers::ALT,
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
            KeyModifiers::ALT | KeyModifiers::SHIFT,
        ];
        codes
            .into_iter()
            .flat_map(|c| mods.iter().map(move |m| KeyEvent::new(c, *m)))
            .collect()
    }

    /// The contexts a key can be live in. Every probe is run in all of them,
    /// because a binding that is live in exactly one — `Home` on an empty box,
    /// `Tab` under an open menu — is still a binding.
    fn every_context() -> Vec<Ctx> {
        vec![
            Ctx::IDLE,
            Ctx::IDLE.typing(),
            Ctx::IDLE.with_menu(),
            Ctx::IDLE.typing().with_menu(),
            Ctx::IDLE.with_page(),
        ]
    }

    /// What any text box does, drawn nowhere because it needs no caption: a
    /// printable character, the two deletes, the two arrows, and the two ends
    /// of the line — all unmodified, all reaching the editor and changing it.
    ///
    /// `Shift` counts as unmodified here because `Shift+A` is how a capital
    /// letter arrives, not a chord.
    fn is_ordinary_typing(key: KeyEvent, answer: &Answer) -> bool {
        let plain = !key.modifiers.contains(KeyModifiers::CONTROL)
            && !key.modifiers.contains(KeyModifiers::ALT);
        let motion = matches!(
            key.code,
            KeyCode::Char(_)
                | KeyCode::Backspace
                | KeyCode::Delete
                | KeyCode::Left
                | KeyCode::Right
                | KeyCode::Home
                | KeyCode::End
        );
        plain && motion && matches!(answer, Answer::Edit(Action::Edit))
    }

    /// Half one, forwards: nothing on screen is dead.
    ///
    /// Drive every chord the panel draws through the real decoders and check
    /// it lands where the row says. This is the test the fork's `QUICK HELP`
    /// table should have had — four of its six rows would fail it.
    #[test]
    fn every_drawn_key_is_answered() {
        // Reads the active keymap through `Trigger::Bound`, so it holds the
        // lock every installing test holds, and starts from the compiled map.
        let _lock = keymap::test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        keymap::install(keymap::Keymap::compiled());
        restore_keymap();
        for binding in CHAT.drawn {
            match binding.trigger {
                Trigger::Terminal(_) => {}
                Trigger::AltAny => {
                    // The class, not a letter: which letters are bound is the
                    // catalogue's, and it changes.
                    let got = answer(
                        Chord {
                            code: KeyCode::Char('s'),
                            mods: KeyModifiers::ALT,
                        },
                        binding.ctx,
                    );
                    assert!(
                        matches(&got, Expect::Tool),
                        "the panel advertises Alt+key and the decoder answers {got:?}"
                    );
                }
                Trigger::Bound(action) => {
                    let chord = chord_for_action(action);
                    let got = answer(chord, binding.ctx);
                    assert!(
                        got.is_live(),
                        "{} is drawn in QUICK HELP and nothing answers it \
                         (a dead key on screen is the whole defect this \
                         table exists to make impossible)",
                        chord.label()
                    );
                    let expected = match action {
                        KeyAction::Sidebar => PaneKey::Sidebar,
                        KeyAction::Help => PaneKey::Help,
                        other => panic!("{other:?} is drawn as a bound row and has no pane key"),
                    };
                    assert!(
                        matches(&got, Expect::Pane(expected)),
                        "{} is drawn as {action:?} and reaches {got:?} instead",
                        chord.label()
                    );
                }
                Trigger::Keys(keys) => {
                    for (chord, expect) in keys {
                        let got = answer(*chord, binding.ctx);
                        assert!(
                            got.is_live(),
                            "{} is drawn in QUICK HELP and nothing answers it \
                             (a dead key on screen is the whole defect this \
                             table exists to make impossible)",
                            chord.label()
                        );
                        assert!(
                            matches(&got, *expect),
                            "{} is drawn as {:?} and reaches {got:?} instead",
                            chord.label(),
                            expect
                        );
                    }
                }
            }
        }
    }

    /// A rebound chord must change the derived label, or QUICK HELP lies.
    ///
    /// The decoder half waits on `input::pane_key` consulting the keymap;
    /// this test pins the label half, which is what [`Trigger::Bound`] exists
    /// for.
    #[test]
    fn a_rebound_action_updates_the_drawn_label() {
        // `with_keymap` below takes the lock; taking it here too would deadlock.
        let sidebar = CHAT
            .drawn
            .iter()
            .find(|b| matches!(b.trigger, Trigger::Bound(KeyAction::Sidebar)))
            .expect("sidebar is keymap-bound");
        restore_keymap();
        assert_eq!(sidebar.label(), "Ctrl+B");
        with_keymap(r#"{ "bindings": { "sidebar": "ctrl+x" } }"#, || {
            assert_eq!(sidebar.label(), "Ctrl+X");
        });
    }

    /// Half one, backwards, and the half people skip: nothing that works is a
    /// secret.
    ///
    /// Probe several thousand keystrokes across every context. Anything that
    /// does something must be in the drawn table or in [`UNDRAWN`] with a
    /// reason. Adding a binding and forgetting to draw it fails here — which
    /// is what stops the next `Ctrl-U` from happening.
    #[test]
    fn every_handled_key_is_drawn_or_deliberately_hidden() {
        // Reads the active keymap through `Trigger::Bound`, so it holds the
        // lock every installing test holds, and starts from the compiled map.
        let _lock = keymap::test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        keymap::install(keymap::Keymap::compiled());
        let mut orphans: Vec<String> = Vec::new();
        for key in every_keystroke() {
            for ctx in every_context() {
                let got = outcome(key, ctx);
                if !got.0.is_live() || is_ordinary_typing(key, &got.0) {
                    continue;
                }
                let chord = Chord {
                    code: key.code,
                    mods: key.modifiers,
                };
                if accounted(chord) || is_a_tolerated_spelling(key, ctx, &got) {
                    continue;
                }
                orphans.push(format!("{} -> {:?} (ctx {ctx:?})", chord.label(), got.0));
            }
        }
        orphans.sort();
        orphans.dedup();
        assert!(
            orphans.is_empty(),
            "these keys do something and appear on no rendered surface and in \
             no undrawn record:\n  {}",
            orphans.join("\n  ")
        );
    }

    /// What a keystroke did, in full: where it went **and** what it left the
    /// line looking like.
    ///
    /// The line matters. `Answer::Edit(Action::Edit)` is the answer for both
    /// "moved the cursor one column" and "moved it one word", so comparing
    /// answers alone would call a future word-motion binding identical to the
    /// plain arrow it is not.
    fn outcome(key: KeyEvent, ctx: Ctx) -> (Answer, String, usize) {
        let mut editor = Editor::default();
        if !ctx.editor_empty {
            editor.paste("a goal worth several words");
        }
        let got = resolve(&mut editor, key, ctx);
        (got, editor.text(), editor.cursor())
    }

    /// Whether a keystroke is an accounted binding — or ordinary typing —
    /// wearing a modifier the decoders demonstrably do not consult.
    ///
    /// **This was written after the probe found seventy of them**, and it is
    /// the finding rather than a way around it: none of the four decoders
    /// compares the modifier set, only the bits an arm names. `pane_key`
    /// matches `KeyCode::PageUp` whatever is held, so `Shift+PgUp` and
    /// `Ctrl+Shift+PgUp` scroll too; `menu_key` takes `Ctrl+Tab` as `Tab`; and
    /// `KeyCode::Char('u') if ctrl` fires for `Ctrl+Shift+U`, which is a
    /// Unicode-entry chord on several Linux input methods. The same tolerance
    /// is why `Ctrl+Left` moves one column and `Ctrl+Backspace` deletes one
    /// character, where every other editor moves and deletes a word — nothing
    /// claims those chords, so the plain arm takes them.
    ///
    /// Mostly the tolerance is what a user wants: `Shift+PgUp` scrolling is
    /// not a bug. Recording seventy spellings as separate rows would be, so
    /// the rule is derived rather than listed — a keystroke passes only if
    /// dropping some of its modifiers leaves the line in **exactly** the same
    /// state, and that shorter chord is either in a table or ordinary typing.
    /// Bind `Ctrl+Left` to word-motion tomorrow and the cursor lands
    /// somewhere else, so it stops passing and has to be drawn or recorded.
    ///
    /// What none of this establishes is whether those spellings *reach* Emma.
    /// A terminal may eat `Ctrl+Shift+PgUp` before Emma sees it; that is on
    /// the human certification list, not here.
    fn is_a_tolerated_spelling(key: KeyEvent, ctx: Ctx, got: &(Answer, String, usize)) -> bool {
        // A capital with a modifier is Shift held on the same key, spelled by
        // the terminal in the character rather than in the modifier bits
        // (Ctrl+Shift+B arrives as `Char('B')` with CONTROL). The keymap
        // lookup folds case, so the chord answers; it is the Shift tolerance
        // above in another spelling, and is tolerated on the same terms: the
        // lower-case chord must answer identically and be accounted for.
        if let KeyCode::Char(c) = key.code {
            if c.is_ascii_uppercase() && !key.modifiers.is_empty() {
                let lowered = Chord {
                    code: KeyCode::Char(c.to_ascii_lowercase()),
                    mods: key.modifiers,
                };
                let theirs = outcome(lowered.press(), ctx);
                // The lowered chord may itself carry a spare Shift bit (some
                // terminals send both), so it gets the modifier tolerance too.
                if theirs == *got
                    && (accounted(lowered) || is_a_tolerated_spelling(lowered.press(), ctx, got))
                {
                    return true;
                }
            }
        }
        let held = [
            KeyModifiers::CONTROL,
            KeyModifiers::ALT,
            KeyModifiers::SHIFT,
        ];
        for mask in 1u8..8 {
            let mut fewer = key.modifiers;
            for (i, m) in held.iter().enumerate() {
                if mask & (1 << i) != 0 {
                    fewer.remove(*m);
                }
            }
            if fewer == key.modifiers {
                continue;
            }
            let shorter = Chord {
                code: key.code,
                mods: fewer,
            };
            let theirs = outcome(shorter.press(), ctx);
            if theirs != *got {
                continue;
            }
            if accounted(shorter) || is_ordinary_typing(shorter.press(), &theirs.0) {
                return true;
            }
        }
        false
    }

    /// Is this chord named by either table?
    fn accounted(chord: Chord) -> bool {
        if chord.mods.contains(KeyModifiers::ALT)
            && !chord.mods.contains(KeyModifiers::CONTROL)
            && matches!(chord.code, KeyCode::Char(_))
            && CHAT
                .drawn
                .iter()
                .any(|b| matches!(b.trigger, Trigger::AltAny))
        {
            return true;
        }
        let bound = CHAT.drawn.iter().any(|b| {
            let Trigger::Bound(action) = b.trigger else {
                return false;
            };
            chord_for_action(action) == chord
        });
        if bound {
            return true;
        }
        let drawn = CHAT.drawn.iter().any(|b| match b.trigger {
            Trigger::Keys(keys) => keys.iter().any(|(c, _)| *c == chord),
            _ => false,
        });
        drawn || UNDRAWN.iter().any(|(c, _, _)| *c == chord)
    }

    /// The modifier tolerance, stated rather than assumed.
    ///
    /// Every drawn chord answers the same way with `Shift` additionally held,
    /// because no decoder compares the whole modifier set — it tests the bits
    /// an arm names. That is a fact about the code with a user-visible edge:
    /// the panel prints `PgUp`, and `Shift+PgUp` scrolls as well.
    ///
    /// Pinned here so making a decoder strict is a decision somebody takes
    /// deliberately, with this test going red, rather than a tightening that
    /// quietly removes a spelling a user had got used to.
    #[test]
    fn the_decoders_do_not_compare_the_whole_modifier_set() {
        for binding in CHAT.drawn {
            let Trigger::Keys(keys) = binding.trigger else {
                continue;
            };
            for (chord, _) in keys {
                let with_shift = Chord {
                    code: chord.code,
                    mods: chord.mods | KeyModifiers::SHIFT,
                };
                assert_eq!(
                    answer(with_shift, binding.ctx),
                    answer(*chord, binding.ctx),
                    "{} and {} stopped agreeing; a decoder now compares the \
                     whole modifier set, which changes what a user can press",
                    chord.label(),
                    with_shift.label()
                );
            }
        }
    }

    /// The other guard on [`UNDRAWN`]: a hidden binding that has stopped
    /// working is a row nobody would ever notice, because there is nothing on
    /// screen to disagree with it.
    ///
    /// Without this, deleting the `Ctrl-W` arm would leave the record claiming
    /// a binding that no longer exists and every other test green.
    #[test]
    fn nothing_hidden_is_already_dead() {
        for (chord, ctx, why) in UNDRAWN {
            let got = answer(*chord, *ctx);
            assert!(
                got.is_live(),
                "{} is recorded as deliberately undrawn ({why}) and nothing \
                 answers it any more",
                chord.label()
            );
        }
    }

    /// And the third guard: a key cannot be in both tables. A row that is
    /// drawn *and* recorded as hidden means one of the two is stale, and the
    /// pair stops meaning anything.
    #[test]
    fn nothing_is_both_drawn_and_hidden() {
        for (chord, _, _) in UNDRAWN {
            let drawn = CHAT.drawn.iter().any(|b| match b.trigger {
                Trigger::Keys(keys) => keys.iter().any(|(c, _)| c == chord),
                _ => false,
            });
            assert!(
                !drawn,
                "{} is both drawn in QUICK HELP and recorded as undrawn",
                chord.label()
            );
        }
    }

    /// The label is derived, so the panel cannot spell a key it does not mean.
    ///
    /// This is the guarantee that makes the fork's `Ctrl+k`/`Ctrl-U` mismatch
    /// unrepresentable rather than merely tested: there is no field to put a
    /// wrong spelling in. The assertion here is that the derivation reproduces
    /// what is already on screen — a change to it is a change to the panel,
    /// and should be argued rather than slipped in.
    #[test]
    fn the_printed_label_comes_from_the_chord() {
        // Reads the active keymap through `Trigger::Bound`, so it holds the
        // lock every installing test holds, and starts from the compiled map.
        let _lock = keymap::test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        keymap::install(keymap::Keymap::compiled());
        assert_eq!(Chord::ctrl('b').label(), "Ctrl+B");
        assert_eq!(Chord::plain(KeyCode::Char('/')).label(), "/");
        assert_eq!(Chord::plain(KeyCode::PageUp).label(), "PgUp");
        assert_eq!(
            CHAT.drawn
                .iter()
                .map(|b| b.label())
                .collect::<Vec<_>>()
                .join(" "),
            "/ Enter Esc PgUp/PgDn Ctrl+Up/Dn Home/End Ctrl+B Ctrl+/ Alt+key Ctrl+C Ctrl+D Alt+Q Shift+drag",
            "the derived labels no longer match what the panel has always said"
        );
    }

    /// The rows the sidebar renders are the rows this table holds, one for
    /// one. `app::keymap` returns `CHAT.hints()`, so this is the seam that
    /// makes "the declaration is the thing that draws" true rather than
    /// aspirational.
    #[test]
    fn the_panel_renders_this_table_and_nothing_else() {
        let _lock = keymap::test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        keymap::install(keymap::Keymap::compiled());
        restore_keymap();
        let hints = CHAT.hints();
        assert_eq!(hints.len(), CHAT.drawn.len());
        assert_eq!(
            hints,
            crate::term::app::keymap(),
            "the sidebar's QUICK HELP panel is no longer drawn from this table, \
             which is the exact arrangement §11 of the fork inventory is about"
        );
    }

    /// `Esc` carries two claims on one row, and their order is the interesting
    /// part: with the menu up it dismisses the menu, and only with the menu
    /// shut does it leave a page. Inverting that would take the key away from
    /// the inner thing, which is what a user pressing it expects to reach.
    #[test]
    fn esc_dismisses_the_menu_before_it_leaves_a_page() {
        let esc = Chord::plain(KeyCode::Esc);
        assert_eq!(
            answer(esc, Ctx::IDLE.with_menu().with_page()),
            Answer::Menu(MenuKey::Dismiss),
        );
        assert_eq!(answer(esc, Ctx::IDLE.with_page()), Answer::LeavePage);
    }

    /// The `/` row says "command menu", and the editor answering `Edit` is
    /// only half of that: what makes the row true is that the typed `/`
    /// actually opens the menu.
    ///
    /// Asserted against the real [`Menu`] rather than trusted, because
    /// `Expect::Edits` on its own would pass for any printable character.
    #[test]
    fn the_slash_row_opens_the_real_menu() {
        let mut editor = Editor::default();
        let got = resolve(
            &mut editor,
            Chord::plain(KeyCode::Char('/')).press(),
            Ctx::IDLE,
        );
        assert_eq!(got, Answer::Edit(Action::Edit));
        let mut menu = Menu::for_project(&["review"]);
        menu.sync(&editor.text(), false);
        assert!(
            menu.is_open(),
            "the panel advertises / as the command menu and typing it opens nothing"
        );
    }

    /// The tool row is a promise about the catalogue, not about a letter. It
    /// fails if the catalogue ever offers nothing launchable — at which point
    /// `Alt+key` on screen would be advertising an empty set.
    #[test]
    fn the_tool_row_has_something_to_launch() {
        let entries = crate::usertools::catalogue(std::path::Path::new("."));
        let live: Vec<char> = entries
            .iter()
            .filter(|e| e.available)
            .map(|e| e.key)
            .collect();
        assert!(
            !live.is_empty(),
            "QUICK HELP advertises Alt+key and the catalogue has nothing available"
        );
        for key in live {
            if key == '/' {
                // Search is the command menu the character already opens; the
                // sidebar shows `/` for it rather than a chord.
                continue;
            }
            let got = answer(
                Chord {
                    code: KeyCode::Char(key),
                    mods: KeyModifiers::ALT,
                },
                Ctx::IDLE,
            );
            assert_eq!(
                got,
                Answer::Tool(key),
                "the sidebar lists Alt+{key} and the decoder drops it"
            );
        }
    }

    /// A question on screen kills every key aimed away from it — the design's
    /// §4.6 rule — except the two that scroll, because reading the evidence
    /// above an approval prompt is exactly when scrolling matters.
    ///
    /// Here rather than in `input.rs` because it is a claim about the *order*
    /// this module states: a `prompt_pending` that reached the tool branch
    /// would launch a shell from under an approval gate.
    #[test]
    fn a_pending_question_kills_everything_but_the_scroll_keys() {
        let pending = Ctx {
            prompt_pending: true,
            ..Ctx::IDLE
        };
        assert_eq!(
            answer(Chord::plain(KeyCode::PageUp), pending),
            Answer::Pane(PaneKey::PageUp)
        );
        assert_eq!(
            answer(Chord::ctrl('b'), pending),
            Answer::Edit(Action::Ignore),
            "Ctrl-B toggled the sidebar under a prompt"
        );
        assert_eq!(
            answer(
                Chord {
                    code: KeyCode::Char('s'),
                    mods: KeyModifiers::ALT
                },
                pending
            ),
            Answer::Edit(Action::Ignore),
            "a tool chord launched under a prompt"
        );
        assert_eq!(
            answer(Chord::plain(KeyCode::Esc), pending.with_page()),
            Answer::Edit(Action::Ignore),
            "Esc left a page under a prompt"
        );
    }

    /// Windows reports releases as well as presses. Every decoder guards it
    /// individually; this asserts the composition does, because a release that
    /// slipped past one guard and into another would type or launch twice.
    #[test]
    fn a_release_edge_reaches_nothing() {
        for chord in [
            Chord::plain(KeyCode::PageUp),
            Chord::ctrl('b'),
            Chord::plain(KeyCode::Esc),
            Chord::plain(KeyCode::Char('a')),
            Chord {
                code: KeyCode::Char('s'),
                mods: KeyModifiers::ALT,
            },
        ] {
            let mut key = chord.press();
            key.kind = KeyEventKind::Release;
            let mut editor = Editor::default();
            let got = resolve(&mut editor, key, Ctx::IDLE.with_page());
            assert!(
                !got.is_live(),
                "{} acted on the release edge: {got:?}",
                chord.label()
            );
        }
    }
}
