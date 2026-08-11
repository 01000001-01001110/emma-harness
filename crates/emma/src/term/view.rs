//! The inline viewport: the few rows at the bottom of the screen that Emma
//! owns, and everything they can show.
//!
//! **Nothing above these rows is ours.** The transcript is written with
//! `Terminal::insert_before`, which scrolls the whole screen the way any
//! program's output scrolls it — so the terminal wraps it, the terminal keeps it
//! in scrollback, and a mouse can select it. This file draws only the rows below
//! that, and it redraws them from scratch on every change.
//!
//! # Why the question lives here
//!
//! Emma asks before it writes a file or runs a command, and on the first real
//! run that question scrolled off under the tool output that followed it.
//! `approval.rs` argues that a prompt the user cannot evaluate manufactures
//! consent; a prompt the user cannot *see* is the same defect with the argument
//! already made. A viewport is the structural fix rather than a careful one: the
//! rows are below the scrolling region of the screen, so there is no amount of
//! output that can push the question off them.
//!
//! # Why the answer keys are the loudest thing on screen
//!
//! They are the only part the user has to act on. Everything else in the panel
//! is there so they can decide; the keys are how they say it. They are the one
//! place in Emma that paints a background — see [`Palette::chip`].
//!
//! # What is deliberately small
//!
//! The panel shows as much of the preview as fits and says how much it left
//! out. The whole preview is in the transcript immediately above, where it
//! scrolls and can be selected and read at length. Putting all of it in the
//! viewport would mean a viewport whose height changes with the size of a diff,
//! and a viewport that resizes under a running program is the class of bug that
//! ate the last two attempts at this file.

use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph, Widget, Wrap};

use super::menu::{MenuView, PLACEHOLDER};
use super::palette::Role;
use super::render::{fit, Skin, Status};

/// What the viewport is doing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    /// Waiting for a goal.
    Idle,
    /// A goal is running. The input box is still there — what is typed goes to
    /// the *next* goal — but the hint changes and the clock runs.
    Working,
}

/// A question waiting for an answer, and everything needed to answer it.
#[derive(Debug, Clone, Default)]
pub struct Prompt {
    /// `Approve Bash` / `Allow network`.
    pub title: String,
    /// The command, the diff, the path and size, the host: whatever
    /// `approval::preview` composed.
    pub preview: Vec<String>,
    /// `[y] yes` and friends, in the order they are offered.
    pub keys: Vec<(String, String)>,
    /// The question itself, kept verbatim so the transcript record of the
    /// answer reads back exactly as it was asked.
    pub question: String,
}

/// Everything the viewport draws, as data.
///
/// Pure: no terminal, no locks. [`Frame`](super::frame::Frame) owns one of
/// these behind a mutex and hands it to `Terminal::draw`; a test renders it into
/// a bare [`Buffer`] and reads the cells back.
#[derive(Debug, Clone)]
pub struct View {
    pub skin: Skin,
    pub status: Status,
    pub mode: Mode,
    /// The line the user is typing, and where the cursor is in it, counted in
    /// characters.
    pub input: String,
    pub cursor: usize,
    /// Assistant text written since the last newline — the line still being
    /// streamed. It lives here rather than in the transcript because the
    /// transcript is written a whole line at a time: a fragment inserted above
    /// the viewport could never be extended. It is moved into the transcript,
    /// verbatim and exactly once, the moment its newline arrives.
    pub partial: String,
    pub prompt: Option<Prompt>,
    /// The command menu, when `/` has one open. A snapshot pushed here by the
    /// reader thread — see [`super::menu`], which owns the decisions, and
    /// [`super::input`], which owns the keys. `None` is the ordinary case and
    /// draws nothing, which is what keeps a pending question unaffected: the
    /// menu is never synced while one is up.
    pub menu: Option<MenuView>,
}

impl View {
    pub fn new(skin: Skin) -> Self {
        Self {
            skin,
            status: Status::default(),
            mode: Mode::Idle,
            input: String::new(),
            cursor: 0,
            partial: String::new(),
            prompt: None,
            menu: None,
        }
    }

    /// Draw, and say where the cursor belongs.
    ///
    /// The cursor is the one piece of terminal state this returns rather than
    /// sets: `Terminal::draw` wants it after the frame is rendered, and a
    /// viewport with no visible cursor is a text field nobody can tell they are
    /// typing into.
    pub fn render(&self, area: Rect, buf: &mut Buffer) -> Option<Position> {
        if area.height == 0 || area.width == 0 {
            return None;
        }
        let [status, body, hint] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .areas(area);

        self.skin
            .status(status.width, &self.status)
            .render(status, buf);
        Line::from(Span::styled(
            fit(
                &self.hint(),
                usize::from(hint.width),
                self.skin.glyphs.ellipsis,
            ),
            self.skin.palette.dim(),
        ))
        .render(hint, buf);

        match &self.prompt {
            Some(prompt) => self.render_prompt(prompt, body, buf),
            None => self.render_input(body, buf),
        }
    }

    /// What the bottom row says. It is the only permanently visible place the
    /// two things nobody can guess are written down.
    ///
    /// Composed from [`Glyphs::sep`](super::render::Glyphs) rather than written
    /// out, because a console on a legacy code page renders a literal `·` as
    /// mojibake — the same reason the glyph table has two halves.
    fn hint(&self) -> String {
        let sep = self.skin.glyphs.sep;
        match self.mode {
            Mode::Working => {
                format!("Ctrl-C interrupts this goal {sep} what you type now runs next")
            }
            Mode::Idle => format!("type a goal {sep} / lists commands {sep} Ctrl-C interrupts"),
        }
    }

    /// The ordinary case: the menu or the streamed prose above, an input box
    /// under it.
    ///
    /// **The box is laid out first and never gives up its rows.** Whatever is
    /// above it — a paragraph arriving, a command menu — takes what is left,
    /// which is what makes "the menu must not push the input off screen on a
    /// short viewport" a property of the layout rather than of a check
    /// somebody has to remember.
    fn render_input(&self, area: Rect, buf: &mut Buffer) -> Option<Position> {
        // Three rows for the box, whatever is left above it. When there is not
        // even three, the box wins: a stream with nowhere to type is worse than
        // a stream nobody can see the last line of.
        let box_rows = 3.min(area.height);
        let [above, boxed] =
            Layout::vertical([Constraint::Min(0), Constraint::Length(box_rows)]).areas(area);

        if above.height > 0 {
            match &self.menu {
                // The menu wins the space while it is up: the user is picking a
                // command, and the prose they are covering is one keystroke —
                // Esc — from being back.
                Some(menu) => self.render_menu(menu, above, buf),
                None if !self.partial.is_empty() => {
                    // The tail, not the head: the interesting end of a line
                    // being streamed is the end.
                    let text = Line::from(Span::styled(
                        self.partial.clone(),
                        self.skin.palette.style(Role::Text),
                    ));
                    let rows = super::render::rows_used(&text, above.width);
                    Paragraph::new(text)
                        .wrap(Wrap { trim: false })
                        .scroll((rows.saturating_sub(above.height), 0))
                        .render(above, buf);
                }
                None => {}
            }
        }

        if boxed.height < 3 {
            return None;
        }
        // Accent rather than grey: this is the one thing on screen the user is
        // meant to act on, and a widget that is the same weight as its own
        // decoration reads as decoration.
        let block = Block::bordered()
            .border_set(self.skin.glyphs.border)
            .border_style(self.skin.palette.style(Role::Accent));
        let inner = block.inner(boxed);
        block.render(boxed, buf);
        let prefix = "> ";
        // An empty box says what to put in it. A new user's first question is
        // "what does this take", and the answer was previously nowhere except a
        // note that had already scrolled away.
        let typed = if self.input.is_empty() {
            Span::styled(PLACEHOLDER.to_string(), self.skin.palette.dim())
        } else {
            Span::styled(self.input.clone(), self.skin.palette.style(Role::Text))
        };
        Line::from(vec![
            Span::styled(prefix.to_string(), self.skin.palette.bold(Role::Accent)),
            typed,
        ])
        .render(inner, buf);
        Some(cursor_at(inner, prefix.chars().count() + self.cursor))
    }

    /// The command menu, drawn directly above the input box.
    ///
    /// It takes the rows that are there and no more. When the list is longer
    /// than the space, the last row says how many were left out rather than the
    /// list quietly stopping — the same rule the approval preview follows, and
    /// for the same reason: a list that is silently a third of itself cannot be
    /// used to answer "what can I type".
    fn render_menu(&self, menu: &MenuView, area: Rect, buf: &mut Buffer) {
        let width = usize::from(area.width);
        let mut room = usize::from(area.height);
        // The note is the point of the menu when a project has no commands, so
        // it keeps a row whenever there is more than one.
        let note = menu.note.clone().filter(|_| room > 1);
        if note.is_some() {
            room -= 1;
        }
        let overflow = menu.rows.len() > room;
        // The "N more" row is worth a line only when there is a line to spare.
        // With exactly one row left, one real command beats a line saying how
        // many commands there were.
        let more = overflow && room >= 2;
        let body = if more { room - 1 } else { room };
        let mut lines: Vec<Line<'static>> = Vec::new();
        if body > 0 {
            // Scrolled just far enough to keep the highlighted row on screen.
            let start = menu
                .selected
                .saturating_sub(body - 1)
                .min(menu.rows.len().saturating_sub(body));
            for (i, (name, about)) in menu.rows.iter().enumerate().skip(start).take(body) {
                let chosen = i == menu.selected;
                let name = format!(" /{name} ");
                let about = fit(
                    about,
                    width.saturating_sub(name.chars().count() + 1),
                    self.skin.glyphs.ellipsis,
                );
                lines.push(Line::from(vec![
                    Span::styled(
                        name,
                        if chosen {
                            self.skin.palette.chip(Role::Accent)
                        } else {
                            self.skin.palette.bold(Role::Accent)
                        },
                    ),
                    Span::styled(format!(" {about}"), self.skin.palette.dim()),
                ]));
            }
            if more {
                lines.push(Line::from(Span::styled(
                    format!(
                        "{} {} more",
                        self.skin.glyphs.ellipsis,
                        menu.rows.len() - body
                    ),
                    self.skin.palette.dim(),
                )));
            }
        }
        if let Some(note) = note {
            lines.push(Line::from(Span::styled(
                fit(&note, width, self.skin.glyphs.ellipsis),
                self.skin.palette.dim(),
            )));
        }
        Paragraph::new(lines).render(area, buf);
    }

    /// A question, its evidence, and the keys that answer it.
    ///
    /// **The border is dropped before the evidence is.** A box costs two rows,
    /// and on a short window those two rows are the difference between showing
    /// the command and showing nothing but a title and a `[y]`. A prompt the
    /// user cannot evaluate manufactures consent — that argument is older than
    /// this file — so the decoration goes first and the title becomes an
    /// ordinary line.
    fn render_prompt(&self, prompt: &Prompt, area: Rect, buf: &mut Buffer) -> Option<Position> {
        let title = Span::styled(
            format!(" {} ", prompt.title),
            self.skin.palette.bold(Role::Warn),
        );
        let inner = if area.height >= 5 {
            let block = Block::bordered()
                .border_set(self.skin.glyphs.border)
                .border_style(self.skin.palette.style(Role::Warn))
                .title(title.clone());
            let inner = block.inner(area);
            block.render(area, buf);
            inner
        } else {
            let [head, rest] =
                Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(area);
            Line::from(title).render(head, buf);
            rest
        };
        if inner.height == 0 {
            return None;
        }

        // The keys and the answer share the last row, and they get it first:
        // whatever else has to go, the way to answer the question does not.
        let [evidence, answer] =
            Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(inner);

        if evidence.height > 0 {
            let room = usize::from(evidence.height);
            let shown = self.preview_lines(prompt, room, evidence.width);
            Paragraph::new(shown).render(evidence, buf);
        }

        let mut spans: Vec<Span<'static>> = Vec::new();
        for (key, label) in &prompt.keys {
            spans.push(Span::styled(
                format!(" {key} "),
                self.skin.palette.chip(Role::Warn),
            ));
            spans.push(Span::styled(
                format!(" {label}  "),
                self.skin.palette.style(Role::Text),
            ));
        }
        let typed_at = spans
            .iter()
            .map(|s| s.content.chars().count())
            .sum::<usize>();
        spans.push(Span::styled(
            self.input.clone(),
            self.skin.palette.bold(Role::Accent),
        ));
        Line::from(spans).render(answer, buf);
        Some(cursor_at(answer, typed_at + self.cursor))
    }

    /// As much of the preview as fits, and an honest line about the rest.
    ///
    /// The *last* line of the note says how many were dropped rather than
    /// dropping them silently, because the whole argument for showing a preview
    /// is that the user can evaluate what they are approving — and a preview
    /// that is quietly a third of a diff cannot be evaluated.
    fn preview_lines(&self, prompt: &Prompt, room: usize, width: u16) -> Vec<Line<'static>> {
        let cut = |s: &String| {
            Line::from(Span::styled(
                fit(s, usize::from(width), self.skin.glyphs.ellipsis),
                self.skin.palette.style(Role::Text),
            ))
        };
        if prompt.preview.len() <= room {
            return prompt.preview.iter().map(cut).collect();
        }
        let keep = room.saturating_sub(1);
        let mut out: Vec<Line<'static>> = prompt.preview.iter().take(keep).map(cut).collect();
        out.push(Line::from(Span::styled(
            format!(
                "{} {} more lines, all of it above",
                self.skin.glyphs.ellipsis,
                prompt.preview.len() - keep
            ),
            self.skin.palette.dim(),
        )));
        out
    }
}

/// Where the cursor lands, clamped so a long line cannot put it off the row.
fn cursor_at(area: Rect, offset: usize) -> Position {
    let x = area.x + (offset as u16).min(area.width.saturating_sub(1));
    Position::new(x, area.y)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::term::palette::{Level, Palette};
    use crate::term::render::UNICODE;

    fn view() -> View {
        let mut v = View::new(Skin::new(Palette::new(Level::Truecolor), UNICODE));
        v.status.model = "claude-opus-4".into();
        v.status.cwd = "E:\\emma".into();
        v
    }

    /// Everything drawn, as one string per row, so a test can read the
    /// viewport the way a person does.
    fn draw(v: &View, width: u16, height: u16) -> (Vec<String>, Option<Position>) {
        let area = Rect::new(0, 0, width, height);
        let mut buf = Buffer::empty(area);
        let cursor = v.render(area, &mut buf);
        let rows = (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect();
        (rows, cursor)
    }

    #[test]
    fn the_idle_viewport_is_a_status_line_an_input_box_and_a_hint() {
        let (rows, cursor) = draw(&view(), 60, 8);
        assert!(rows[0].contains("emma"), "{rows:?}");
        assert!(rows.iter().any(|r| r.contains('>')), "{rows:?}");
        assert!(rows[7].contains("/ lists commands"), "{rows:?}");
        assert!(cursor.is_some(), "nothing showed the user where they type");
        // An empty box says what it takes. Without this the first thing a new
        // user sees is a rounded rectangle with a caret in it.
        assert!(
            rows.iter().any(|r| r.contains("describe a goal")),
            "the empty box had no placeholder: {rows:?}"
        );
    }

    #[test]
    fn what_is_being_typed_appears_in_the_box_with_the_cursor_after_it() {
        let mut v = view();
        v.input = "port the middleware".into();
        v.cursor = v.input.chars().count();
        let (rows, cursor) = draw(&v, 60, 8);
        assert!(
            rows.iter().any(|r| r.contains("port the middleware")),
            "{rows:?}"
        );
        // Two columns for the border and the box's own `> `, then the text.
        assert_eq!(cursor.unwrap().x, 1 + 2 + 19);
    }

    /// R6, structurally: the question is in the viewport, and the viewport is
    /// not where output goes. There is no sequence of writes that can move it.
    #[test]
    fn a_pending_question_shows_the_evidence_and_the_keys_to_answer_it() {
        let mut v = view();
        v.prompt = Some(Prompt {
            title: "Approve Bash".into(),
            preview: vec!["$ rm -rf build/".into(), "  in E:\\emma".into()],
            keys: vec![
                ("y".into(), "yes".into()),
                ("n".into(), "no".into()),
                ("a".into(), "always Bash".into()),
            ],
            question: "allow? ".into(),
        });
        let (rows, cursor) = draw(&v, 60, 9);
        let all = rows.join("\n");
        assert!(all.contains("Approve Bash"), "{all}");
        // The thing being approved, not a summary of it.
        assert!(all.contains("rm -rf build/"), "{all}");
        // …and every key on offer.
        for key in ["y", "n", "a"] {
            assert!(
                all.contains(&format!(" {key} ")),
                "key {key} missing: {all}"
            );
        }
        assert!(all.contains("always Bash"), "{all}");
        assert!(cursor.is_some());
    }

    /// The keys are the one thing that must never be pushed off, so they are
    /// laid out first and the evidence takes what is left.
    #[test]
    fn a_huge_preview_loses_lines_rather_than_the_answer_keys() {
        let mut v = view();
        v.prompt = Some(Prompt {
            title: "Approve Edit".into(),
            preview: (0..40).map(|i| format!("  - line {i}")).collect(),
            keys: vec![("y".into(), "yes".into()), ("n".into(), "no".into())],
            question: "allow? ".into(),
        });
        let (rows, _) = draw(&v, 60, 9);
        let all = rows.join("\n");
        assert!(
            all.contains(" y "),
            "the answer keys were pushed off: {all}"
        );
        // …and says what it left out rather than trailing off, pointing at the
        // transcript above, where the whole thing is.
        assert!(all.contains("more lines, all of it above"), "{all}");
    }

    #[test]
    fn streamed_prose_shows_its_tail_above_the_box() {
        let mut v = view();
        v.partial = "a".repeat(300);
        let (rows, _) = draw(&v, 40, 10);
        // Five rows of stream, three of box, one status, one hint — and the
        // stream is showing the end of the text rather than the start.
        assert!(rows[1].starts_with('a'), "{rows:?}");
        assert!(rows[5].starts_with('a'), "{rows:?}");
    }

    #[test]
    fn the_hint_says_what_ctrl_c_does_while_a_goal_is_running() {
        let mut v = view();
        v.mode = Mode::Working;
        let (rows, _) = draw(&v, 70, 8);
        assert!(rows[7].contains("Ctrl-C"), "{rows:?}");
        assert!(rows[7].contains("runs next"), "{rows:?}");
    }

    /// The smallest viewport Emma will draw still shows what is being approved.
    ///
    /// Five rows leaves three for the panel, and a border would eat two of
    /// them — leaving a title and a `[y]` and no statement of what `y` does.
    /// That is the prompt this project already argued is worse than none, so
    /// the box goes and the command stays.
    #[test]
    fn the_shortest_viewport_drops_the_border_rather_than_the_evidence() {
        let mut v = view();
        v.prompt = Some(Prompt {
            title: "Approve Bash".into(),
            preview: vec!["$ rm -rf build/".into()],
            keys: vec![("y".into(), "yes".into()), ("n".into(), "no".into())],
            question: "allow? ".into(),
        });
        let (rows, cursor) = draw(&v, 48, 5);
        let all = rows.join(
            "
",
        );
        assert!(all.contains("Approve Bash"), "{all}");
        assert!(all.contains("rm -rf build/"), "the evidence went: {all}");
        assert!(all.contains(" y "), "{all}");
        assert!(!all.contains(UNICODE.border.top_left), "{all}");
        assert!(cursor.is_some());
    }

    /// A window that is barely there must not panic, and must not draw a box
    /// with no room to type in.
    #[test]
    fn a_viewport_with_almost_no_rows_still_draws_something() {
        for height in 0..5u16 {
            let (_rows, _cursor) = draw(&view(), 20, height);
        }
    }

    // -----------------------------------------------------------------------
    // The command menu
    //
    // The decisions are in `super::menu` and tested there. What is asserted
    // here is only what drawing can get wrong: where it goes, what it does to
    // the input box, and what it does on a viewport with no room in it.
    // -----------------------------------------------------------------------

    fn menu_view() -> View {
        let mut m = crate::term::menu::Menu::for_project(&["review", "ship"]);
        m.sync("/", false);
        let mut v = view();
        v.input = "/".into();
        v.cursor = 1;
        v.menu = m.view();
        v
    }

    #[test]
    fn the_menu_draws_above_the_input_box_with_every_command_and_what_it_does() {
        let (rows, cursor) = draw(&menu_view(), 60, 10);
        let all = rows.join("\n");
        for name in ["/exit", "/quit", "/review", "/ship"] {
            assert!(all.contains(name), "{name} is missing: {all}");
        }
        assert!(all.contains("end this session"), "{all}");
        // Nothing invented for a harness command that came without one.
        assert!(all.contains(crate::term::menu::NO_DESCRIPTION), "{all}");
        // …and the box is still under it, three rows from the hint, with the
        // cursor on the row you type into.
        assert!(rows[7].contains('>'), "{rows:?}");
        assert_eq!(cursor.unwrap().y, 7);
    }

    /// The constraint that would otherwise be found by a user on a laptop: a
    /// menu that eats the box is a menu you cannot type your way out of.
    #[test]
    fn a_short_viewport_keeps_the_input_box_and_shortens_the_menu() {
        // Seven rows: one status, one hint, three for the box, two for the
        // menu — which is one command and an honest count of the rest.
        let (rows, cursor) = draw(&menu_view(), 60, 7);
        let all = rows.join("\n");
        assert!(
            rows.iter().any(|r| r.contains(UNICODE.border.top_left)),
            "the menu pushed the input box off: {rows:?}"
        );
        assert!(cursor.is_some(), "there was nowhere to type");
        assert!(all.contains("/exit"), "{all}");
        // Fewer entries fit, so it says how many it left out rather than
        // stopping silently.
        assert!(all.contains("3 more"), "{all}");

        // One row less, and the count is what goes: a real command is worth
        // more than a line saying how many commands there were.
        let (rows, cursor) = draw(&menu_view(), 60, 6);
        assert!(
            rows.iter().any(|r| r.contains(UNICODE.border.top_left)),
            "{rows:?}"
        );
        assert!(cursor.is_some());
        assert!(rows.join("\n").contains("/exit"), "{rows:?}");
    }

    /// A project that defines nothing still gets an answer to "what can I
    /// type", and the answer says where its own commands would go.
    #[test]
    fn a_project_with_no_commands_shows_the_builtins_and_where_the_rest_would_live() {
        let mut m = crate::term::menu::Menu::for_project(&[]);
        m.sync("/", false);
        let mut v = view();
        v.menu = m.view();
        let all = draw(&v, 70, 10).0.join("\n");
        assert!(all.contains("/exit"), "{all}");
        assert!(all.contains(".emma/commands/"), "{all}");
    }

    /// The rule from `approval.rs`: a question outranks everything. The reader
    /// never syncs a menu while a prompt is up, and if one somehow survived,
    /// the prompt is still what gets drawn.
    #[test]
    fn a_pending_question_is_drawn_rather_than_the_menu() {
        let mut v = menu_view();
        v.prompt = Some(Prompt {
            title: "Approve Bash".into(),
            preview: vec!["$ rm -rf build/".into()],
            keys: vec![("y".into(), "yes".into()), ("n".into(), "no".into())],
            question: "allow? ".into(),
        });
        let all = draw(&v, 60, 9).0.join("\n");
        assert!(all.contains("Approve Bash"), "{all}");
        assert!(
            !all.contains("/review"),
            "the menu drew over a question: {all}"
        );
    }

    #[test]
    fn a_menu_on_a_viewport_with_no_room_above_the_box_draws_nothing_and_panics_at_nothing() {
        for height in 0..6u16 {
            let (_rows, _cursor) = draw(&menu_view(), 24, height);
        }
    }
}
