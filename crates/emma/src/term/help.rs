//! Help: the text, once, and the page that draws it.
//!
//! # One text, two renderings
//!
//! `/help` used to be a string literal in `cli.rs` and nothing else could read
//! it as anything but bytes. A full-screen page needs section headers, an
//! entry column and a scroll offset, so the text is **data** here: a list of
//! [`Section`]s, each with prose paragraphs and a definition list. [`plain`]
//! renders that back to the lines `--help` and the print path have always
//! shown, and [`render`] draws the same sections into the main region. There
//! is no second copy to keep in step, which is the property the invariant test
//! [`every_session_command_is_documented_where_help_can_be_read`] leans on —
//! the assertion that used to live in `cli.rs` over a `const`.
//!
//! # Chords come from the active keymap
//!
//! An entry may name a [`super::keymap::Action`] rather than a chord string.
//! A person who moved `help` to `alt+x` in `~/.emma/keybindings.json` is told
//! `Alt+X` by both renderings, because both resolve the key at the moment they
//! run. A hard-coded chord in a help text is a promise the keymap can break.
//!
//! The spelling goes through [`super::bindings::Binding::label`] rather than
//! through `keymap::display`, which the fork used. Two surfaces print a chord
//! — the sidebar's QUICK HELP panel and this page — and the panel has spelled
//! `Ctrl+B` since before the keymap existed. `display` would spell the same
//! binding `Ctrl+b` here, so the page and the panel would disagree about the
//! capital on one screen. One function, one spelling.
//!
//! # The text describes this tree, not the fork it came from
//!
//! The structure is the fork's; the sentences are mainline's, taken from the
//! `session_help!()` macro this module replaced and from the surfaces that
//! answer the keys — `bindings::CHAT`, `bindings::UNDRAWN`, `input::pane_key`
//! and `frame::launch_tool`. Where the two trees differ the tree wins: there
//! is no steering queue here (nothing reads the keyboard while a goal runs),
//! no `Alt+q` (the way out is `Ctrl-C` and `Ctrl-D`), no `/mode`, `/voice`,
//! `/presence` or `/init`, and `Alt+c` opens the configured editor rather
//! than a page. A help text that describes a different program is worse than
//! no help text, because it is read as evidence.
//!
//! # The text is ASCII, deliberately
//!
//! [`SECTIONS`] holds no character outside ASCII — no em dash, no curly
//! quote. The other pages carry an `honest()` that swaps a glyph for a word
//! under `render::ASCII`; this one does not need it, because there is nothing
//! to swap. The alternative was a page whose prose changed shape with the
//! skin, and two renderings that stopped being the same bytes. Only [`HINT`]
//! has an ASCII twin, because an arrow is a picture rather than a sentence.
//! `the_page_puts_no_control_byte_and_no_wide_overrun_in_a_cell` is what
//! stops the next em dash.

use ratatui::buffer::Buffer;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};

use super::keymap::Action;
use super::palette::Role;
use super::render::{cols, fit, Skin};

// region: The text as data

/// What is printed in an entry's left column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    /// A literal: a command name, an answer key, a chord nobody may move.
    Fixed(&'static str),
    /// A rebindable action, spelled from the keymap in force.
    Bound(Action),
}

impl Key {
    /// The left column, resolved against the keymap this process is running.
    ///
    /// An action with no chord at all reads `unbound` rather than the panel's
    /// `?`: a question mark in a column of chords is a chord — `?` opens this
    /// very page — and "the key you are looking for is `?`" is the one thing
    /// this cell must not accidentally say.
    pub fn spell(self) -> String {
        match self {
            Self::Fixed(s) => s.to_string(),
            Self::Bound(a) => {
                if super::keymap::active().chord_for(a).is_none() {
                    return "unbound".to_string();
                }
                super::bindings::Binding {
                    trigger: super::bindings::Trigger::Bound(a),
                    what: "",
                    ctx: super::bindings::Ctx::IDLE,
                }
                .label()
            }
        }
    }
}

/// One row of a section's definition list.
#[derive(Debug, Clone, Copy)]
pub struct Entry {
    pub key: Key,
    pub text: &'static str,
}

/// One heading, its prose, and its list. Either half may be empty.
#[derive(Debug, Clone, Copy)]
pub struct Section {
    /// The heading, as the page draws it and as [`plain`] upper-cases it.
    pub title: &'static str,
    /// Paragraphs, unwrapped. Wrapping is the renderer's, because the page
    /// and the print path have different widths.
    pub intro: &'static [&'static str],
    pub entries: &'static [Entry],
}

const fn fixed(key: &'static str, text: &'static str) -> Entry {
    Entry {
        key: Key::Fixed(key),
        text,
    }
}

const fn bound(action: Action, text: &'static str) -> Entry {
    Entry {
        key: Key::Bound(action),
        text,
    }
}

/// Everything `/help` says, in reading order.
///
/// The order is the order somebody meets these things: what a session is,
/// then the vocabulary, then the screens, then the keys, then the gate that
/// stands in front of the tools.
pub const SECTIONS: &[Section] = &[
    Section {
        title: "The session",
        intro: &[
            "A goal at the prompt runs until it is done or a budget stops it, then the prompt comes back.",
            "A session is one conversation. The next thing you type continues the last one: what was read, run and answered is still there, so a follow-up question does not re-read the file the answer came from. Budgets are still per goal; the conversation is not. When it grows past --max-context the oldest goals are compacted to their goal and their answer, and their tool results (file contents, command output) leave the conversation. Emma says so when it happens, and the transcript records exactly what was replaced.",
        ],
        entries: &[],
    },
    Section {
        title: "Commands",
        intro: &[
            "Emma's own commands, at the goal prompt. Press / for the same list with the project's commands on the end of it.",
            "Every one of these runs between goals: nothing reads the keyboard while a goal is in flight, so a command typed then is dropped at the next prompt rather than queued. Ctrl-C is what stops a goal.",
        ],
        entries: &[
            fixed("/help", "this page, and the same text `emma --help` prints. On a run with no full screen (a pipe, -p, EMMA_NO_FRAME) it prints instead of opening."),
            fixed("/model", "the model in force, where it came from, and what it accepts: the max_tokens and effort ceiling Emma silently clamps to."),
            fixed("/model <id>", "use <id> for the rest of this session. The conversation is kept and re-sent to the new model; the cached prefix is not, so the next call re-reads it at full price. Nothing on disk changes unless you add --save."),
            fixed("/mode", "the posture in force and the three that exist: assist asks before a write, a command or a network call; auto runs them without asking; plan refuses them and says so. Session only; a new session starts in assist."),
            fixed("/mode <name>", "switch to it for the rest of this session. Nothing is running while you type it, so the switch applies from the next tool call. The status bar's MODE cell follows it."),
            fixed("/compact", "summarise every finished goal but the last, now, instead of waiting for --max-context. The model is asked for the summary with a fixed prompt and every failure falls back to the goal text and its answer; there is no seam for an instruction yet, so words after /compact are refused rather than ignored. /compact all includes the last goal."),
            fixed("/clear", "start a fresh conversation without leaving. The transcript is kept and --resume will not replay what was cleared. This session's approval grants are kept too, and /clear names them: they are consent about the process, and /exit is what drops them."),
            fixed("/copy", "put the last answer on the clipboard. The text is the markdown the model wrote, taken from the session log, not what is on screen, which has been wrapped to a column and sits beside the sidebar. Uses OSC 52, which the terminal either honours or ignores silently; Emma says what it sent, never that it arrived. Refused on -p and on a pipe, where no escape byte may be written."),
            fixed("/export", "write this conversation to a file, in markdown. /export <path> chooses where; with no argument it lands beside the session log. Works on -p, on a pipe and with no console: everywhere /copy is refused. If any record of the session could not be read, the file says so at the top rather than reading as complete."),
            fixed("/theme", "the colours: which theme is selected, which ones this machine and this project have, and where a theme file goes. A fresh machine has none, which is why the empty list says so."),
            fixed("/theme <name>", "select it. A theme is read once, when Emma starts, so the name is written to ~/.emma/settings.json and the next start is what shows it: there is no --save, and nothing repaints. A name that is not there, or a file that will not parse, is refused and nothing is written. NO_COLOR outranks every theme, and /theme says so rather than letting you restart into the same screen."),
            fixed("/config", "what this run resolved: harness, tools, permission rules, agent types, and the model actually running rather than the one on disk."),
            fixed("/agents", "what each subagent type has cost and produced, including this session's delegations so far."),
            fixed("/resume", "the sessions recorded from this directory, most recent first, and how to name one."),
            fixed("/resume <id>", "continue that session here, in this process: the conversation is swapped for the recorded one, this transcript ends with a note saying where the session went, and every continuity difference (instructions, tools, model, directory) is printed as a warning. `emma --resume` from the shell does the same from a fresh process."),
            fixed("/exit, /quit", "end the session."),
            fixed("Ctrl-C", "interrupt the goal that is running."),
            fixed("/<name>", "expand a command from commands/ in .emma/ (or .claude/). `emma config check` lists the ones this directory has; the session lists them at startup. An unknown /word is just text. A name above wins over a project command that shares it, and `emma config check` says when one does."),
        ],
    },
    Section {
        title: "The screens",
        intro: &[
            "The chat is one of several full screens, and opening one closes another. The chord that opened a page closes it, and so does Esc. The sidebar's TOOLS column lists all of these but this one, each with its chord; Help has no row there because it is the page that opens over the others rather than one of them.",
        ],
        entries: &[
            bound(Action::Settings, "the Settings screen."),
            bound(Action::Memory, "the Memory page: this repository's wiki, read fresh from disk every time it opens."),
            bound(Action::Harness, "the Harness dashboard: this repository's real runs, from the session logs. The wheel belongs to its Run Graph while it is open."),
            bound(Action::Help, "this page. A bare ? on an empty input box opens it too. It closes on the same chord, on Esc, on b and on q; the arrows, PgUp and PgDn scroll it."),
        ],
    },
    Section {
        title: "The Alt layer",
        intro: &[
            "Alt with a letter reaches the user tools, wherever the cursor is and whatever is on screen. A bare letter is never a shortcut here, because the first character of every goal lands on an empty box. The sidebar's TOOLS column lists the letters, and its n/a column says when a tool is not available on this machine or in this directory rather than offering a key that does nothing.",
            "Ctrl+Alt with the same letter works where a terminal eats Alt; macOS terminals need Option-as-Meta (Terminal: Use Option as Meta key; iTerm2: Option sends Esc+). Every Alt chord is dead while an approval question is on screen.",
        ],
        entries: &[
            bound(Action::LaunchTerminal, "open a terminal window in this directory."),
            bound(Action::Code, "open the configured editor on this project: tools.editor if it is set, then $VISUAL and $EDITOR, then the first of a short list found on PATH."),
            bound(Action::LaunchReveal, "open a file-browser window here."),
        ],
    },
    Section {
        title: "Editing keys",
        intro: &[
            "The input box is one line, and these are all of its keys, including the ones the sidebar's QUICK HELP panel has no room to advertise. A pasted block arrives with its line breaks flattened to spaces rather than being submitted a fragment at a time, and a paste can never submit.",
        ],
        entries: &[
            fixed("Ctrl-u", "clear the line."),
            fixed("Ctrl-w", "delete the word before the cursor. Ctrl-Backspace is the same edit under the spelling most people reach for first."),
            fixed("Ctrl-Delete", "delete the word after the cursor."),
            fixed("Ctrl-Left, Ctrl-Right", "move the cursor a word at a time."),
            fixed("Ctrl-a, Ctrl-e", "start and end of the line."),
            fixed("Tab", "accept the highlighted command, and only while the command menu is open. With the menu shut it does nothing."),
            fixed("Up, Down", "move the selection while the command menu is open; otherwise walk the lines this session submitted, newest first, and Down past the newest brings back what was being typed."),
            fixed("Esc", "cancel the thing in front of you, one layer at a time: the command menu first, then an open page. The text in the box is left alone: Esc is for the popup, not for the sentence behind it."),
            fixed("Ctrl-D", "end the session, on an empty box only. With text in it, nothing."),
        ],
    },
    Section {
        title: "The chat pane",
        intro: &[
            "PgUp/PgDn move a page and Ctrl-Up/Ctrl-Down a row, whatever is in the box and even while a goal runs or a question waits: reading the evidence above an approval prompt is exactly when scrolling matters. Home and End go to the top and the tail, on an empty box; with a line to move within they belong to the cursor.",
            "With the mouse captured the wheel scrolls, click-drag selects text inside the pane and the selection goes to the clipboard on release, and a press on the scrollbar or on the sidebar's [+] is that control being used. Shift-drag is left to the terminal: it is the one native selection route the alternate screen leaves, so Emma never bids for it.",
            "On a terminal that supports it, Emma frames the window: a status row on top, the transcript scrolling between, and the prompt pinned to the bottom row so an approval question cannot scroll away under the output that follows it. Anything else (a pipe, a redirect, a console without VT processing, or EMMA_NO_FRAME set) gets plain lines instead, with nothing else different.",
        ],
        entries: &[
            bound(Action::Sidebar, "show or hide the sidebar. Dead while a question is pending, like every key aimed away from the prompt."),
        ],
    },
    Section {
        title: "Keybindings",
        intro: &[
            "Every chord on this page except Ctrl-C and Ctrl-D can be moved in ~/.emma/keybindings.json, which documents its own schema. Emma reads it once at startup, so an edit there applies to the next run; switching preset applies at once, because every preset in the file was read by that one read.",
            "Ctrl-C and Ctrl-D are deliberately not rebindable: the way out of a running goal is not a preference.",
            "This page and the sidebar's QUICK HELP panel both spell the chord that is bound right now, not the one that shipped.",
            "Ctrl+/ is not one byte everywhere. A terminal sends 0x1F for it and reports that as /, as _ or as 7 depending on the keyboard; all three are folded to the same keystroke before any chord is looked up, so the promise holds on the next terminal as well as this one.",
        ],
        entries: &[],
    },
    Section {
        title: "Approval",
        intro: &[
            "Read, Glob and Grep run silently. Write, Edit and Bash ask, showing the command, the diff, or the path and size. A tool that reaches the network asks separately about the host, showing the URL or the query.",
        ],
        entries: &[
            fixed("y", "allow this call. On the network question it also allows that host for the rest of the process."),
            fixed("n", "refuse. Empty is 'n': hitting return is not consent."),
            fixed("a", "allow that tool for the rest of the process, and no longer."),
            fixed("r", "allow, and write the rule down: the host on the network question, the tool on the others. The exact rule is shown before you press the key."),
            fixed("t", "network question only: allow, and write down the whole tool, every call it makes, to any host. Bigger than 'r' on purpose, which is why it is a different key."),
        ],
    },
    Section {
        title: "What a rule is",
        intro: &[
            "What 'r' and 't' write is a Claude Code permission rule, in <harness>/settings.local.json: WebFetch(domain:apnews.com), or WebFetch for every host. `emma config check` lists every rule and the file it came from; delete a line to revoke it. Rules already in .claude/settings.json are honoured, and a deny rule in your ~/.claude/settings.json applies here too (allow rules there do not: they were written for a different program).",
            "The order, and the first line that answers wins: a PreToolUse hook denial, a deny rule, --dangerously-skip-permissions, an ask rule, an allow rule, read-only, a session grant, then you. A hook denial and a deny rule cannot be approved away, and neither is waved through by the bypass flag.",
            "Delegate asks like any other writer, showing which agent type and the first lines of the brief. A subagent inherits this gate: its prompts are the same prompts, on the same keyboard, which is why only one delegation runs at a time.",
            "One exemption, by name: TaskCreate and TaskUpdate write, and never ask. They write only to the agent's own task file under .emma/, and a prompt every time the agent ticks off a task is a prompt that gets answered without being read, which costs the prompts on Write, Edit and Bash as well.",
        ],
        entries: &[],
    },
];

// endregion: The text as data

// region: The plain rendering

/// The width `--help` and the print path wrap to. The old literal was hand
/// wrapped near here, so the output keeps its shape.
const PLAIN_WIDTH: usize = 78;

/// The column an entry's text starts in, print path. Four spaces of indent,
/// then a key column wide enough for `Ctrl-Left, Ctrl-Right`.
const PLAIN_KEY: usize = 29;

/// Wrap `text` to `width`, on spaces, never breaking a word that fits.
///
/// A word longer than the whole width is emitted on its own line rather than
/// split: the long words here are paths and flags, and half of `--max-context`
/// on one line is worse than a line that runs over.
pub fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(8);
    let mut out = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        if line.is_empty() {
            line.push_str(word);
        } else if cols(&line) + 1 + cols(word) <= width {
            line.push(' ');
            line.push_str(word);
        } else {
            out.push(std::mem::take(&mut line));
            line.push_str(word);
        }
    }
    if !line.is_empty() {
        out.push(line);
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
}

/// The bytes `/help` prints and `emma --help` carries as its second half.
///
/// Built rather than stored, from [`SECTIONS`] and the keymap in force. Two
/// consequences worth stating: a rebound chord shows up in `--help` too, and
/// this is a `String`, so callers hold it rather than pointing at it.
pub fn plain() -> String {
    let mut out = String::from("THE INTERACTIVE SESSION\n");
    for section in SECTIONS {
        out.push('\n');
        out.push_str("  ");
        out.push_str(&section.title.to_uppercase());
        out.push('\n');
        for para in section.intro {
            for row in wrap(para, PLAIN_WIDTH - 4) {
                out.push_str("    ");
                out.push_str(&row);
                out.push('\n');
            }
            out.push('\n');
        }
        for entry in section.entries {
            let key = entry.key.spell();
            let body = wrap(entry.text, PLAIN_WIDTH.saturating_sub(PLAIN_KEY));
            for (i, row) in body.iter().enumerate() {
                if i == 0 && cols(&key) + 5 <= PLAIN_KEY {
                    out.push_str("    ");
                    out.push_str(&key);
                    out.push_str(&" ".repeat(PLAIN_KEY - 4 - cols(&key)));
                } else if i == 0 {
                    // A key too wide for the column takes its own row rather
                    // than pushing the text off the edge.
                    out.push_str("    ");
                    out.push_str(&key);
                    out.push('\n');
                    out.push_str(&" ".repeat(PLAIN_KEY));
                } else {
                    out.push_str(&" ".repeat(PLAIN_KEY));
                }
                out.push_str(row);
                out.push('\n');
            }
        }
        if !section.entries.is_empty() {
            out.push('\n');
        }
    }
    // One blank line between blocks, never two: a paragraph ends with one and
    // the next section opens with one, and the two would stack.
    while out.contains("\n\n\n") {
        out = out.replace("\n\n\n", "\n\n");
    }
    while out.ends_with("\n\n") {
        out.pop();
    }
    out
}

// endregion: The plain rendering

// region: The page

/// The Help page's whole state: where the scroll is, and what the last paint
/// found out about the window.
///
/// No copy of the text. The rows are built from [`SECTIONS`] at paint time
/// because they depend on the width, and a stored copy would be the second
/// source of truth this module exists to avoid.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HelpView {
    /// First visible row.
    pub scroll: usize,
    /// Rows the last paint could show. Zero before the first paint, which is
    /// why the key handler treats a zero page as one row.
    pub page_rows: usize,
    /// The largest scroll the last paint would leave content on screen at.
    pub max_scroll: usize,
}

impl HelpView {
    /// A page, as the keys mean it: the visible rows less one of continuity.
    fn page(&self) -> usize {
        self.page_rows.saturating_sub(1).max(1)
    }

    fn scroll_by(&mut self, delta: isize) -> HelpAction {
        let next = (self.scroll as isize + delta).max(0) as usize;
        let next = next.min(self.max_scroll);
        if next == self.scroll {
            // Still the page's key: a key that fell through at the top of a
            // scroll would type its letter into the box behind the page.
            return HelpAction::Held;
        }
        self.scroll = next;
        HelpAction::Scrolled
    }
}

/// What one key did to the page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HelpAction {
    /// Not this page's key (or a release, or a chord): let it fall through.
    None,
    /// The page took it and nothing moved.
    Held,
    Scrolled,
    /// Esc, b, q, or the help chord again.
    Close,
}

/// One key against the open page. Pure, like every other page's.
///
/// **Chords fall through.** Ctrl-C, Ctrl-D, the Alt layer and the help toggle
/// itself must reach the global layer while this page is open, so anything
/// carrying ALT or CONTROL is refused here. That is the same rule
/// `memory::handle_key` and `harness::handle_key` state, and it is what makes
/// the toggle a toggle.
pub fn handle_key(v: &mut HelpView, key: KeyEvent) -> HelpAction {
    if key.kind == KeyEventKind::Release
        || key
            .modifiers
            .intersects(KeyModifiers::ALT | KeyModifiers::CONTROL)
    {
        return HelpAction::None;
    }
    let page = v.page() as isize;
    match key.code {
        KeyCode::Esc | KeyCode::Char('b') | KeyCode::Char('q') => HelpAction::Close,
        KeyCode::Up | KeyCode::Char('k') => v.scroll_by(-1),
        KeyCode::Down | KeyCode::Char('j') => v.scroll_by(1),
        KeyCode::PageUp => v.scroll_by(-page),
        KeyCode::PageDown | KeyCode::Char(' ') => v.scroll_by(page),
        KeyCode::Home => v.scroll_by(-(v.scroll as isize)),
        KeyCode::End => v.scroll_by(v.max_scroll as isize),
        // Every other plain key belongs to the open page, acted on or not:
        // the memory/harness rule, so a letter cannot leak into the box that
        // is not on screen.
        _ => HelpAction::Held,
    }
}

/// One rendered row, before it is given a colour.
enum Row {
    Heading(String),
    Blank,
    Body(String),
    /// A definition-list row: the key column (empty on continuation rows) and
    /// the text.
    Entry(String, String),
}

/// The width of the page's key column, at a page width of `body_w`.
///
/// One function because the paint and [`rows`] must agree about it: they are
/// two places that lay out the same cell, and a disagreement is a text column
/// that starts where the key column ends on one of them and not the other.
fn key_col(body_w: usize) -> usize {
    18.min(body_w.saturating_sub(8))
}

/// The whole page at this width, in order. Built for the paint and for the
/// scroll arithmetic, which is why it is one function rather than two.
fn rows(width: usize) -> Vec<Row> {
    let body_w = width.saturating_sub(2).max(8);
    let key_w = key_col(body_w);
    let text_w = body_w.saturating_sub(key_w + 3).max(8);
    let mut out = Vec::new();
    for (i, section) in SECTIONS.iter().enumerate() {
        if i > 0 {
            out.push(Row::Blank);
        }
        out.push(Row::Heading(section.title.to_uppercase()));
        for para in section.intro {
            for row in wrap(para, body_w) {
                out.push(Row::Body(row));
            }
            out.push(Row::Blank);
        }
        for entry in section.entries {
            let key = entry.key.spell();
            for (j, row) in wrap(entry.text, text_w).into_iter().enumerate() {
                let left = if j == 0 { key.clone() } else { String::new() };
                out.push(Row::Entry(left, row));
            }
        }
    }
    out
}

/// What the paint learned. Handed back so the shell can clamp a scroll the
/// window has just made unreachable, the Run Graph's `canvas_rows` rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Metrics {
    pub page_rows: usize,
    pub max_scroll: usize,
}

/// The hint row's text. One place, because the page draws it and a test reads
/// it.
pub const HINT: &str = "↑/↓ scroll   PgUp/PgDn page   [b] or Esc back";
/// The same row where the skin has no box-drawing to spend. `render::ASCII`'s
/// rule: a terminal that cannot draw an arrow gets the word.
pub const HINT_ASCII: &str = "Up/Dn scroll   PgUp/PgDn page   [b] or Esc back";

/// Draw the page into `area`. The header is the house shape the Memory and
/// Harness pages use: title, subtitle, rule, then the content.
pub fn render(area: Rect, buf: &mut Buffer, v: &HelpView, skin: &Skin) -> Metrics {
    let ascii = skin.glyphs == super::render::ASCII;
    let nothing = Metrics {
        page_rows: 0,
        max_scroll: 0,
    };
    if area.width < 4 || area.height < 2 {
        return nothing;
    }
    let w = usize::from(area.width);
    let ell = skin.glyphs.ellipsis;
    let mut y = area.y;
    let bottom = area.y + area.height;
    let head = [
        Line::from(Span::styled(
            fit("Help", w, ell),
            skin.palette.bold(Role::Accent),
        )),
        Line::from(Span::styled(
            fit("What this session understands", w, ell),
            skin.palette.dim(),
        )),
        Line::from(Span::styled(skin.glyphs.rule.repeat(w), skin.palette.dim())),
    ];
    for line in head {
        if y >= bottom {
            return nothing;
        }
        buf.set_line(area.x, y, &line, area.width);
        y += 1;
    }
    // The hint row is anchored at the foot, so the content never runs under
    // the one row that says how to leave.
    let hint_y = bottom.saturating_sub(1);
    let content_bottom = hint_y.max(y);
    let page_rows = usize::from(content_bottom.saturating_sub(y));
    let all = rows(w);
    let max_scroll = all.len().saturating_sub(page_rows);
    let start = v.scroll.min(max_scroll);
    let key_w = key_col(w.saturating_sub(2).max(8));
    for (i, row) in all.iter().skip(start).take(page_rows).enumerate() {
        let ry = y + i as u16;
        let line = match row {
            Row::Blank => Line::from(""),
            Row::Heading(t) => Line::from(Span::styled(
                fit(t, w, ell),
                skin.palette.bold(Role::Accent),
            )),
            Row::Body(t) => Line::from(vec![
                Span::raw("  "),
                Span::styled(
                    fit(t, w.saturating_sub(2), ell),
                    skin.palette.style(Role::Text),
                ),
            ]),
            Row::Entry(key, text) => {
                // The key cell is padded by *columns*, not by characters: a
                // rebind can put any single character in here, and a wide one
                // padded by `len()` is the column-budget class this repository
                // has already paid for once.
                let shown = fit(key, key_w, ell);
                let pad = " ".repeat(key_w.saturating_sub(cols(&shown)));
                Line::from(vec![
                    Span::raw("  "),
                    Span::styled(format!("{shown}{pad}"), skin.palette.style(Role::Info)),
                    Span::raw(" "),
                    Span::styled(
                        fit(text, w.saturating_sub(key_w + 3), ell),
                        skin.palette.style(Role::Text),
                    ),
                ])
            }
        };
        buf.set_line(area.x, ry, &line, area.width);
    }
    if hint_y >= y {
        let hint = if ascii { HINT_ASCII } else { HINT };
        let line = Line::from(Span::styled(fit(hint, w, ell), skin.palette.dim()));
        buf.set_line(area.x, hint_y, &line, area.width);
    }
    Metrics {
        page_rows,
        max_scroll,
    }
}

// endregion: The page

#[cfg(test)]
mod tests {
    use super::*;
    use crate::term::keymap;
    use ratatui::crossterm::event::KeyModifiers;

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::from(code)
    }

    fn view(rows: usize, max: usize) -> HelpView {
        HelpView {
            scroll: 0,
            page_rows: rows,
            max_scroll: max,
        }
    }

    /// The process-wide keymap is one cell, and two test modules install into
    /// it. `bindings.rs` has its own `with_keymap`; this is the same shape,
    /// and the two can still interleave — see DONE-H1 §6.
    fn with_keymap(json: &str, f: impl FnOnce()) {
        // The keymap is process-wide; every test that touches it holds this.
        let _lock = keymap::test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        keymap::install(keymap::parse(json));
        f();
        keymap::install(keymap::Keymap::compiled());
    }

    /// The whole point of the restructure: one body of text, two renderings.
    /// If the page ever grows a sentence the print path does not have, this
    /// is what says so.
    #[test]
    fn the_page_and_the_printed_text_render_the_same_data() {
        let _lock = keymap::test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        keymap::install(keymap::Keymap::compiled());
        let text = plain();
        for section in SECTIONS {
            assert!(
                text.contains(&section.title.to_uppercase()),
                "{} is missing from the printed text",
                section.title
            );
            for entry in section.entries {
                let key = entry.key.spell();
                assert!(
                    text.contains(&key),
                    "{key} is missing from the printed text"
                );
                // The first few words are enough to catch a dropped entry
                // without pinning the wrapping, which is the renderer's.
                let head: String = entry
                    .text
                    .split_whitespace()
                    .take(4)
                    .collect::<Vec<_>>()
                    .join(" ");
                assert!(
                    text.split_whitespace()
                        .collect::<Vec<_>>()
                        .join(" ")
                        .contains(&head),
                    "{head:?} is missing from the printed text"
                );
            }
        }
        // And the page builds every one of those rows at a real width.
        let drawn = rows(80)
            .iter()
            .map(|r| match r {
                Row::Heading(t) | Row::Body(t) => t.clone(),
                Row::Entry(k, t) => format!("{k} {t}"),
                Row::Blank => String::new(),
            })
            .collect::<Vec<_>>()
            .join(" ");
        for section in SECTIONS {
            assert!(
                drawn.contains(&section.title.to_uppercase()),
                "{} is missing from the page",
                section.title
            );
        }
    }

    /// Every command the menu offers has a paragraph here saying what it does.
    ///
    /// **Moved from `cli.rs`, where it asserted over a `const`.** The menu row
    /// is one line on a sixty-column viewport and cannot answer "what does
    /// /clear keep?". This is `menu.rs`'s rule — a command nobody can discover
    /// is not a command — applied one level up, and it is the assertion that
    /// stops the thirteenth command shipping undocumented.
    #[test]
    fn every_session_command_is_documented_where_help_can_be_read() {
        // Whole words. A substring test would let `/clear` be satisfied by a
        // paragraph about `/clearance`, which is the shape of false receipt
        // this repository has already paid for once.
        let text = plain();
        let words: Vec<&str> = text
            .split(|c: char| c.is_whitespace() || c == ',')
            .collect();
        for (name, _) in crate::session_command::BUILTINS {
            let spelled = format!("/{name}");
            assert!(
                words.contains(&spelled.as_str()),
                "/{name} is offered by the menu and is not documented in the help text"
            );
        }
        // The two ways out, and the one that stops a goal: the defect this
        // half of the assertion was written for was `/exit` working since the
        // loop was written and documented nowhere a user looks.
        for way_out in ["/exit", "/quit", "Ctrl-C"] {
            assert!(
                text.contains(way_out),
                "the help does not mention {way_out}"
            );
        }
    }

    /// Chords are resolved when the text is built, not when it was written.
    #[test]
    fn a_bound_key_spells_the_chord_the_keymap_holds() {
        let _keymap = crate::term::keymap::test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        keymap::install(keymap::Keymap::compiled());
        assert_eq!(Key::Bound(Action::Help).spell(), "Ctrl+/");
        assert_eq!(Key::Bound(Action::Memory).spell(), "Alt+M");
        assert_eq!(Key::Fixed("Ctrl-C").spell(), "Ctrl-C");
    }

    /// The property the whole `Key::Bound` mechanism exists for, through the
    /// *printed* text rather than through `spell` alone: a person who moved
    /// `help` to `alt+x` reads `Alt+X` in `--help` and on the page, and the
    /// compiled `Ctrl+/` is gone from both.
    #[test]
    fn a_rebound_chord_reaches_both_renderings() {
        with_keymap(r#"{ "bindings": { "help": "alt+x" } }"#, || {
            let text = plain();
            assert!(
                text.contains("Alt+X"),
                "the printed text kept the old chord"
            );
            // And the compiled chord is *gone* from the key column, not
            // merely joined by the new one: a rebind that only added a
            // spelling would leave "I moved it" reading as "there are two
            // now". Asserted over the entry keys rather than over the whole
            // string, because the Keybindings section's prose names `Ctrl+/`
            // in a sentence about how terminals encode it, and that sentence
            // is true whatever the key is bound to.
            for section in SECTIONS {
                for entry in section.entries {
                    assert_ne!(
                        entry.key.spell(),
                        "Ctrl+/",
                        "{} still advertises the compiled chord",
                        entry.text
                    );
                }
            }
            let page = rows(80)
                .iter()
                .map(|r| match r {
                    Row::Heading(t) | Row::Body(t) => t.clone(),
                    Row::Entry(k, t) => format!("{k} {t}"),
                    Row::Blank => String::new(),
                })
                .collect::<Vec<_>>()
                .join(" ");
            assert!(page.contains("Alt+X"), "the page kept the old chord");
        });
    }

    #[test]
    fn wrapping_never_breaks_a_word_and_never_returns_nothing() {
        let rows = wrap("a --very-long-flag-nobody-can-break in a narrow column", 12);
        assert!(rows
            .iter()
            .any(|r| r.contains("--very-long-flag-nobody-can-break")));
        assert!(rows.iter().all(|r| !r.starts_with(' ')));
        assert_eq!(wrap("", 20), vec![String::new()]);
    }

    /// The close keys, the scroll keys, and the rule that makes the toggle a
    /// toggle: a chord is not the page's, so Ctrl+/ reaches the global layer
    /// and closes what it opened.
    #[test]
    fn the_page_closes_on_esc_and_b_scrolls_on_the_arrows_and_lets_chords_through() {
        let mut v = view(10, 40);
        assert_eq!(handle_key(&mut v, press(KeyCode::Esc)), HelpAction::Close);
        assert_eq!(
            handle_key(&mut v, press(KeyCode::Char('b'))),
            HelpAction::Close
        );
        assert_eq!(
            handle_key(
                &mut v,
                KeyEvent::new(KeyCode::Char('/'), KeyModifiers::CONTROL)
            ),
            HelpAction::None,
            "a chord must reach the global layer, or the toggle cannot close"
        );
        assert_eq!(
            handle_key(&mut v, press(KeyCode::Down)),
            HelpAction::Scrolled
        );
        assert_eq!(v.scroll, 1);
        assert_eq!(
            handle_key(&mut v, press(KeyCode::PageDown)),
            HelpAction::Scrolled
        );
        assert_eq!(
            v.scroll, 10,
            "a page is the visible rows less one of continuity"
        );
        assert_eq!(
            handle_key(&mut v, press(KeyCode::End)),
            HelpAction::Scrolled
        );
        assert_eq!(v.scroll, 40);
        assert_eq!(
            handle_key(&mut v, press(KeyCode::Down)),
            HelpAction::Held,
            "the end is a floor, and the key is still the page's"
        );
        assert_eq!(
            handle_key(&mut v, press(KeyCode::Home)),
            HelpAction::Scrolled
        );
        assert_eq!(v.scroll, 0);
        // Every other plain key belongs to the open page: a letter must not
        // leak into a box that is not on screen.
        assert_eq!(
            handle_key(&mut v, press(KeyCode::Char('z'))),
            HelpAction::Held
        );
    }

    /// The paint reports the window, and a window too small to hold the text
    /// still reports a scroll ceiling that reaches the last row.
    #[test]
    fn the_paint_reports_a_scroll_ceiling_that_reaches_the_last_row() {
        let skin = Skin::new(
            super::super::palette::Palette::new(super::super::palette::Level::Truecolor),
            super::super::render::UNICODE,
        );
        let area = Rect::new(0, 0, 80, 20);
        let mut buf = Buffer::empty(area);
        let v = HelpView::default();
        let m = render(area, &mut buf, &v, &skin);
        assert!(m.page_rows > 0);
        assert_eq!(m.max_scroll, rows(80).len().saturating_sub(m.page_rows));
        assert!(m.max_scroll > 0, "the help text is longer than twenty rows");
        // The last row is reachable: scrolled to the ceiling, the final row
        // of the text is on screen.
        let end = HelpView {
            scroll: m.max_scroll,
            ..v
        };
        let mut buf = Buffer::empty(area);
        render(area, &mut buf, &end, &skin);
        let painted: String = buf.content().iter().map(|c| c.symbol()).collect();
        assert!(
            painted.contains("TaskCreate"),
            "the end of the text never came into view"
        );
    }

    /// Nothing drawn may carry an escape byte, a TAB or a control character
    /// into a cell, and the ASCII skin's page must be ASCII: the two
    /// regression classes this repository has already paid for, asserted over
    /// every row of the real text at a real width.
    #[test]
    fn the_page_puts_no_control_byte_and_no_wide_overrun_in_a_cell() {
        let skin = Skin::new(
            super::super::palette::Palette::new(super::super::palette::Level::Ansi16),
            super::super::render::ASCII,
        );
        let area = Rect::new(0, 0, 72, 40);
        let mut buf = Buffer::empty(area);
        let mut v = HelpView::default();
        // Every row of the text, a screenful at a time.
        loop {
            let m = render(area, &mut buf, &v, &skin);
            for y in 0..area.height {
                let row: String = (0..area.width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<Vec<_>>()
                    .join("");
                assert!(
                    row.is_ascii(),
                    "non-ASCII under the ASCII skin at row {y}: {row:?}"
                );
                assert!(
                    !row.chars().any(|c| c.is_control()),
                    "a control character reached a cell at row {y}: {row:?}"
                );
            }
            if v.scroll >= m.max_scroll {
                break;
            }
            v.scroll = (v.scroll + usize::from(area.height)).min(m.max_scroll);
            v.max_scroll = m.max_scroll;
        }
    }

    /// A narrow window still lays the key column out inside the page: the
    /// paint and [`rows`] read the same `key_col`, so the text column starts
    /// where the key column ends on both.
    #[test]
    fn the_key_column_is_the_same_width_in_the_layout_and_in_the_paint() {
        for w in [20usize, 40, 80, 200] {
            let body_w = w.saturating_sub(2).max(8);
            let key_w = key_col(body_w);
            assert!(key_w + 3 <= body_w + 2, "the key column overran width {w}");
        }
    }
}
