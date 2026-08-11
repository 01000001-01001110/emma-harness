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
    /// What the configured status program last printed, when one is configured
    /// and has answered. `None` is every other case — nothing configured, the
    /// first run still in flight, the program failed — and every one of them
    /// draws the built-in status, because the built-in line is true and an empty
    /// row reads as a crash. See [`super::statusline`].
    pub custom_status: Option<String>,
    /// Leading spaces the configuration asked for. Claude Code's
    /// `statusLine.padding`; meaningless without `custom_status`.
    pub status_padding: u16,
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
            custom_status: None,
            status_padding: 0,
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
        // **Bottom-up: the thing you type into, then the hint, then the status
        // on the very last row.** The status used to be the first row; the owner
        // asked for what the terminal agents he uses do — the chat bar at the
        // bottom of the window with the status underneath it. The eye ends where
        // the cursor is, and the two rows below the box are the two that never
        // move.
        //
        // **Nothing was added.** These are the same two rows of chrome the old
        // order spent, reordered, so `body` gets exactly the height it got
        // before and every case that fitted still fits — including the five-row
        // minimum, where `body` is three rows and the input box takes all of
        // them. A third fixed row would have cost the shortest viewport the
        // ability to show a command *and* the keys to approve it, which
        // `render_prompt` already surrenders its border to protect.
        //
        // The hint keeps a row of its own rather than being folded into the
        // status. The status is the one line on screen that is *live*, and
        // `Skin::status` earns its honesty by dropping whole fields when it runs
        // out of room; sharing that row with a fixed sentence would mean an
        // advice string deciding whether a measurement fits. Showing the hint
        // only when it "has something to say" was the other candidate and is
        // worse in the hand: the row would come and go as the mode changed,
        // moving the input box under the user's fingers.
        let [body, hint, status] = Layout::vertical([
            Constraint::Min(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .areas(area);

        self.status_row(status.width).render(status, buf);
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

    /// The last row: the configured program's output when there is some, and the
    /// built-in status otherwise.
    ///
    /// **The fallback is not a degraded mode, it is the product.** A status
    /// program is a luxury on top of a line that already carries the model, the
    /// directory, the elapsed clock and two meters measured against real caps.
    /// Every path that is not "a configured program answered" lands here, which
    /// is what makes a broken script cost a note rather than a blank row.
    ///
    /// The reverse of that trade is the one worth stating in the docs: a program
    /// that answers **replaces** the built-in line entirely, so anything it does
    /// not print is simply gone. See `notes/status-line.md`.
    fn status_row(&self, width: u16) -> Line<'static> {
        match &self.custom_status {
            Some(text) => super::statusline::to_line(
                text,
                self.status_padding,
                self.skin.palette.style(Role::Text),
            ),
            None => self.skin.status(width, &self.status),
        }
    }

    /// What the hint row says. It is the only permanently visible place the
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

    /// **The order, and the only test that pins it.** The box you type into is
    /// at the bottom of the window, with the hint and then the status under it.
    ///
    /// Written as "which row is which, relative to the last one" rather than as
    /// three independent `contains` calls over the whole viewport, because the
    /// property *is* the ordering: a test that only asked whether each piece
    /// appeared somewhere would pass the layout this replaced, which had the
    /// status on the first row and the hint on the last.
    #[test]
    fn the_input_box_is_at_the_bottom_with_the_hint_and_then_the_status_beneath_it() {
        let (rows, cursor) = draw(&view(), 60, 8);
        let last = rows.len() - 1;

        // The very last row of the window is the status.
        assert!(rows[last].contains("emma"), "{rows:?}");
        assert!(rows[last].contains("claude-opus-4"), "{rows:?}");
        // Directly above it, the hint.
        assert!(rows[last - 1].contains("/ lists commands"), "{rows:?}");
        // And above *that*, the input box — closed off by its own bottom
        // border, so "above" is the whole box and not just some row with a `>`
        // on it.
        let bottom = rows
            .iter()
            .rposition(|r| r.contains(UNICODE.border.bottom_left))
            .expect("there was no input box at all");
        assert_eq!(
            bottom,
            last - 2,
            "the input box is not immediately above the hint and status: {rows:?}"
        );
        // The row you actually type on is inside that box, and the cursor is on
        // it. This is what would break if the box were laid out from the top.
        let cursor = cursor.expect("nothing showed the user where they type");
        assert_eq!(usize::from(cursor.y), bottom - 1, "{rows:?}");
        assert!(rows[bottom - 1].contains('>'), "{rows:?}");

        // An empty box says what it takes. Without this the first thing a new
        // user sees is a rounded rectangle with a caret in it.
        assert!(
            rows[bottom - 1].contains("describe a goal"),
            "the empty box had no placeholder: {rows:?}"
        );
    }

    /// The row budget did not change, and this is the assertion that says so.
    ///
    /// Moving the status from the first row to the last was a reordering, not an
    /// addition: `body` gets the same height it always got, so everything that
    /// fitted before still fits. The five-row minimum is where that matters —
    /// three rows for the box and nothing to spare — and it is the size
    /// `fallback_reason` and `view_rows` agree is the smallest Emma will draw.
    #[test]
    fn the_reordering_cost_the_body_no_rows_at_the_smallest_viewport_emma_draws() {
        for height in [5u16, 8, 10] {
            let (rows, cursor) = draw(&view(), 60, height);
            let last = rows.len() - 1;
            assert!(
                rows[last].contains("emma"),
                "height {height} lost its status row: {rows:?}"
            );
            // Two rows of chrome, three of box — so a viewport of five still has
            // somewhere to type, which is the property the box is laid out
            // first to guarantee.
            assert!(
                rows.iter().any(|r| r.contains(UNICODE.border.bottom_left)),
                "height {height} had no input box: {rows:?}"
            );
            assert!(cursor.is_some(), "height {height} had nowhere to type");
        }
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
        // Ten rows: five of stream, three of box, then the hint and the status
        // — so the stream starts at the very top of the viewport now that
        // nothing sits above it. And it is showing the *end* of the text rather
        // than the start, which is the point of the test.
        assert!(rows[0].starts_with('a'), "{rows:?}");
        assert!(rows[4].starts_with('a'), "{rows:?}");
        // Nothing spilled onto the box or the two rows under it.
        assert!(!rows[5].starts_with('a'), "{rows:?}");
    }

    #[test]
    fn the_hint_says_what_ctrl_c_does_while_a_goal_is_running() {
        let mut v = view();
        v.mode = Mode::Working;
        let (rows, _) = draw(&v, 70, 8);
        // Second from the bottom: under the box, above the status.
        let hint = rows.len() - 2;
        assert!(rows[hint].contains("Ctrl-C"), "{rows:?}");
        assert!(rows[hint].contains("runs next"), "{rows:?}");
        // …and it is the hint that changed, not the status, which still carries
        // the run's identity while a goal is running.
        assert!(rows[rows.len() - 1].contains("emma"), "{rows:?}");
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
        // …and the box is still under it, with the cursor on the row you type
        // into — which is now the third row from the bottom, because the hint
        // and the status are below the box rather than around it.
        assert!(rows[6].contains('>'), "{rows:?}");
        assert_eq!(cursor.unwrap().y, 6);
        assert!(rows[9].contains("emma"), "the status is not last: {rows:?}");
    }

    /// The constraint that would otherwise be found by a user on a laptop: a
    /// menu that eats the box is a menu you cannot type your way out of.
    #[test]
    fn a_short_viewport_keeps_the_input_box_and_shortens_the_menu() {
        // Seven rows: two for the menu, three for the box, then the hint and
        // the status — so the menu gets one command and an honest count of the
        // rest.
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

    // -----------------------------------------------------------------------
    // The configured status line
    //
    // The decisions — running it, timing it out, parsing its bytes — are in
    // `super::statusline` and tested there. What is asserted here is only what
    // *drawing* can get wrong: which of the two lines lands on the last row,
    // and that nothing a program printed can reach a cell as a control byte.
    // -----------------------------------------------------------------------

    /// A program that answered replaces the built-in line, and replaces all of
    /// it. This is the trade the docs have to state: the model, the directory,
    /// the clock and both meters are Emma's line, and a script that does not
    /// print them has not hidden them — it has taken their row.
    #[test]
    fn a_configured_status_line_takes_the_last_row_and_the_builtin_one_goes() {
        let mut v = view();
        v.custom_status = Some("main ~ 12% ctx".into());
        let (rows, _) = draw(&v, 60, 8);
        let last = rows.len() - 1;
        assert_eq!(rows[last], "main ~ 12% ctx", "{rows:?}");
        // Nothing of the built-in survives anywhere: not a second copy on
        // another row, and not a half of it sharing this one.
        assert!(
            !rows.join("\n").contains("claude-opus-4"),
            "the built-in status was drawn as well: {rows:?}"
        );
        // …and the hint above it is untouched, because the two rows are two
        // rows and this feature does not get to eat the other one.
        assert!(rows[last - 1].contains("/ lists commands"), "{rows:?}");
    }

    /// Every case that is not "a program answered" draws the built-in line:
    /// nothing configured, the first run still in flight, the program failed.
    /// They are one case in the view on purpose — a `None` here has exactly one
    /// meaning, so there is no state in which the row is blank.
    #[test]
    fn without_an_answer_from_a_program_the_builtin_status_is_what_is_drawn() {
        let mut v = view();
        v.custom_status = None;
        // The padding a configuration asked for must not indent a line it does
        // not apply to.
        v.status_padding = 4;
        let (rows, _) = draw(&v, 60, 8);
        let last = rows.len() - 1;
        assert!(rows[last].starts_with("emma"), "{rows:?}");
        assert!(rows[last].contains("claude-opus-4"), "{rows:?}");
    }

    #[test]
    fn the_padding_a_configuration_asked_for_indents_the_row() {
        let mut v = view();
        v.custom_status = Some("x".into());
        v.status_padding = 3;
        let (rows, _) = draw(&v, 60, 8);
        assert_eq!(rows[rows.len() - 1], "   x", "{rows:?}");
    }

    /// **End to end, through the real render path: a status program cannot put
    /// an escape byte into the cell buffer.**
    ///
    /// `super::statusline` proves this of the translator; this proves the
    /// translator is actually the thing on the path. ratatui counts a stored
    /// escape byte as one column and the terminal counts it as none, so one of
    /// them in a cell makes the frame wrong about every row below it — and the
    /// rows below it are the ones this whole design exists to keep in
    /// scrollback.
    #[test]
    fn nothing_a_status_program_prints_can_write_an_escape_into_the_viewport() {
        let mut v = view();
        v.custom_status = Some(
            // Colour, a hyperlink, a cursor move, a screen erase, and the
            // scroll region this project's scrollback defect was made of.
            "\x1b[32mok\x1b[0m \x1b]8;;https://x\x07link\x1b]8;;\x07 \x1b[2J\x1b[5;9H\x1b[2;20r"
                .into(),
        );
        let (rows, _) = draw(&v, 60, 8);
        // Row by row rather than over a joined string: the join's own newlines
        // are control characters, and a test that had to exclude them would be
        // one edit away from excluding the thing it is looking for.
        for row in &rows {
            assert!(
                !row.chars().any(|c| c == '\x1b' || c.is_control()),
                "an escape or control byte reached the cell buffer: {row:?}"
            );
        }
        // …and the text survived, which is what makes dropping the rest a
        // translation rather than a refusal to draw.
        assert!(rows[rows.len() - 1].contains("ok"), "{rows:?}");
        assert!(rows[rows.len() - 1].contains("link"), "{rows:?}");
    }
}
