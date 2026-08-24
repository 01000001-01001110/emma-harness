//! What each kind of event looks like, as styled lines, and nothing else.
//!
//! Pure. No terminal, no locks, no writes — a function from an event to
//! [`Line`]s, which is what makes "a refused call and a failed call must not
//! look the same" a thing a test can assert rather than a thing somebody has to
//! squint at.
//!
//! # Why there is a vocabulary at all
//!
//! Every tool line used to be `●`. A call that ran, a call that failed with a
//! `ToolError`, a call the human declined and a call a `PreToolUse` hook
//! refused all arrived as the same bullet in the same weight, and the only way
//! to tell them apart was to read the sentence after them. These are four
//! different things — one of them is policy the user cannot override — and the
//! screen should say so before anybody reads a word.
//!
//! # Why every distinction is made twice
//!
//! Glyph *and* colour, always. Colour alone fails on a terminal with sixteen of
//! them, on a terminal with none, and on the substantial minority of readers
//! who cannot distinguish the red from the green. Glyph alone is legible and
//! drab. Both together degrade gracefully in either direction, and neither is
//! load-bearing on its own.
//!
//! # Why the fallback shares this file
//!
//! `Term` renders the same [`Line`]s whether it is drawing into a viewport or
//! writing plain lines to a pipe — [`sgr`] and [`plain`] are the two ways out.
//! Two renderers would mean the frame and the fallback drifting apart, and the
//! fallback is the product.

use ratatui::style::{Modifier, Style};
use ratatui::symbols::border;
use ratatui::text::{Line, Span};

use super::palette::{Level, Palette, Role};
use super::theme::Theme;

// region: Glyphs
// ---------------------------------------------------------------------------
// Glyphs
//
// Two sets, because a Windows console whose output code page is not UTF-8
// renders `✓` as two pieces of mojibake, and a transcript that looks broken
// reads as a broken program rather than as a stylistic choice. Detection is
// `super::console_is_utf8`, and it errs towards ASCII.
// ---------------------------------------------------------------------------

/// The marks that separate one kind of event from another.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Glyphs {
    pub goal: &'static str,
    pub tool: &'static str,
    pub ok: &'static str,
    pub err: &'static str,
    pub refused: &'static str,
    pub blocked: &'static str,
    pub kick: &'static str,
    pub end: &'static str,
    pub note: &'static str,
    pub warn: &'static str,
    pub banner: &'static str,
    pub ellipsis: &'static str,
    pub sep: &'static str,
    /// The mark in front of a markdown list item. The source's own `-` is
    /// replaced rather than kept because a rendered bullet is what a list looks
    /// like — see [`super::markdown`].
    pub bullet: &'static str,
    /// One column of a horizontal rule: a thematic break, and the line a code
    /// fence becomes.
    pub rule: &'static str,
    pub border: border::Set,
}

pub const UNICODE: Glyphs = Glyphs {
    goal: "▶",
    tool: "◆",
    ok: "✓",
    err: "✗",
    refused: "⊘",
    blocked: "⊗",
    kick: "↻",
    end: "■",
    note: "·",
    warn: "!",
    banner: "!!",
    ellipsis: "…",
    sep: "·",
    bullet: "•",
    rule: "─",
    border: border::ROUNDED,
};

pub const ASCII: Glyphs = Glyphs {
    goal: ">",
    tool: "*",
    ok: "+",
    err: "x",
    refused: "o",
    blocked: "X",
    kick: "~",
    end: "=",
    note: "-",
    warn: "!",
    banner: "!!",
    ellipsis: "...",
    sep: "|",
    bullet: "-",
    rule: "-",
    border: border::Set {
        top_left: "+",
        top_right: "+",
        bottom_left: "+",
        bottom_right: "+",
        vertical_left: "|",
        vertical_right: "|",
        horizontal_top: "-",
        horizontal_bottom: "-",
    },
};

// endregion: Glyphs

// region: The skin
// ---------------------------------------------------------------------------
// The skin
//
// Palette plus glyphs, and one method per thing that can happen. Every method
// returns lines rather than writing them, so the whole vocabulary is available
// to a test as data.
// ---------------------------------------------------------------------------

/// How many lines of a tool result reach the screen. Unchanged: a screen of
/// JSON is a screen nobody reads.
pub const RESULT_LINES: usize = 8;

#[derive(Debug, Clone, Copy)]
pub struct Skin {
    pub palette: Palette,
    pub glyphs: Glyphs,
}

impl Skin {
    pub fn new(palette: Palette, glyphs: Glyphs) -> Self {
        Self { palette, glyphs }
    }

    /// The same skin with a resolved theme in it, for a swap after boot.
    ///
    /// **The fidelity is passed in, not detected.** The one thing a mid-session
    /// theme change must not do is re-answer "how much colour does this
    /// terminal have" — `Level::of` reads the environment once, and a `/theme`
    /// cannot turn a sixteen-colour console into a truecolor one or overturn
    /// `NO_COLOR`. So this takes the level the caller is already running at and
    /// changes nothing but the colours. Same reason the glyph set is an
    /// argument: it came from a code-page read-back, and a theme has no
    /// business in it.
    pub fn with_theme(level: Level, glyphs: Glyphs, theme: Theme) -> Self {
        Self {
            palette: Palette::with_theme(level, theme),
            glyphs,
        }
    }

    /// A marked line: the glyph in its colour, then the text.
    ///
    /// The glyph carries the weight as well as the colour, because bold is the
    /// one emphasis a sixteen-colour terminal and a colourless one both have.
    fn marked(&self, glyph: &str, role: Role, head: Span<'static>, tail: &str) -> Line<'static> {
        let mut spans = vec![
            Span::styled(glyph.to_string(), self.palette.bold(role)),
            Span::raw(" "),
            head,
        ];
        if !tail.is_empty() {
            spans.push(Span::raw(" "));
            spans.push(Span::styled(
                tail.to_string(),
                self.palette.style(Role::Text),
            ));
        }
        Line::from(spans)
    }

    /// The goal the user typed, opening its turn.
    pub fn goal(&self, text: &str) -> Vec<Line<'static>> {
        vec![self.marked(
            self.glyphs.goal,
            Role::Accent,
            Span::styled(text.trim().to_string(), self.palette.bold(Role::Accent)),
            "",
        )]
    }

    /// A tool is about to run.
    pub fn tool_started(&self, name: &str, head: &str) -> Vec<Line<'static>> {
        vec![self.marked(
            self.glyphs.tool,
            Role::Info,
            Span::styled(name.to_string(), self.palette.bold(Role::Info)),
            head,
        )]
    }

    /// A tool ran. The body is indented under its call and dimmed: it is
    /// evidence, not prose, and the eye should be able to skip it.
    pub fn tool_ok(&self, body: &str, truncated: bool, reason: Option<&str>) -> Vec<Line<'static>> {
        let mut out = self.body(body, Role::Dim);
        if truncated {
            // Distinct from "more lines than fit on screen" below: this one
            // says the *tool* stopped early, so what the model got is also
            // incomplete. Conflating the two would tell a user their output was
            // merely abbreviated when it was actually cut.
            //
            // And when the tool said which cap bound, that is the line, not a
            // paraphrase of it. "output was truncated by the tool" on a page
            // whose text was nowhere near its limit sent a reader looking at
            // the wrong number entirely; the tool's own sentence names the
            // right one. It is left whole rather than shortened to fit —
            // `body` already wraps, and a remedy cut in half is not a remedy.
            let note = match reason {
                Some(r) => format!("  {} truncated: {r}", self.glyphs.ellipsis),
                None => format!(
                    "  {} output was truncated by the tool, which did not say by which limit",
                    self.glyphs.ellipsis
                ),
            };
            out.push(Line::from(Span::styled(
                note,
                self.palette.style(Role::Warn),
            )));
        }
        out
    }

    /// A tool failed with a `ToolError`. Red, and the glyph says so before the
    /// words do.
    pub fn tool_failed(&self, name: &str, detail: &str) -> Vec<Line<'static>> {
        let mut out = vec![self.marked(
            self.glyphs.err,
            Role::Err,
            Span::styled(format!("{name} failed"), self.palette.bold(Role::Err)),
            "",
        )];
        out.extend(self.body(detail, Role::Err));
        out
    }

    /// The call was refused. Not an error — nothing broke — so it is yellow and
    /// its own glyph, and the sentence says who decided.
    ///
    /// **The clause comes from the verdict, because this used to say "you
    /// declined it" on every path.** Four of the six do not involve a person at
    /// all: a `deny` rule, a `-p` run with nobody to ask, a prompt that reached
    /// end of input, and a network call that named no host. Found by running the
    /// real binary — a `-p` run refused `Bash` correctly and then told the
    /// operator they had declined it, three lines above the detail saying there
    /// was nobody to ask.
    ///
    /// `tool_blocked` below already carries this argument in the other
    /// direction: a reader who is told the wrong decider goes looking for a
    /// prompt that never existed.
    pub fn tool_refused(&self, name: &str, because: &str) -> Vec<Line<'static>> {
        vec![self.marked(
            self.glyphs.refused,
            Role::Warn,
            Span::styled(format!("{name} refused"), self.palette.bold(Role::Warn)),
            because,
        )]
    }

    /// A `PreToolUse` hook denied it. Red, because this one is policy: no
    /// answer at the prompt could have allowed it, and a user who reads this as
    /// "I could have said yes" will go looking for a prompt that never comes.
    pub fn tool_blocked(&self, name: &str, reason: &str) -> Vec<Line<'static>> {
        let mut out = vec![self.marked(
            self.glyphs.blocked,
            Role::Err,
            Span::styled(
                format!("{name} blocked by policy"),
                self.palette.bold(Role::Err),
            ),
            "",
        )];
        out.extend(self.body(reason, Role::Err));
        out
    }

    /// The loop nudging a model that stopped without finishing.
    pub fn kick(&self, n: u32, max: u32) -> Vec<Line<'static>> {
        vec![self.marked(
            self.glyphs.kick,
            Role::Warn,
            Span::styled(
                format!("not done yet — nudge {n}/{max}"),
                self.palette.style(Role::Warn),
            ),
            "",
        )]
    }

    /// A goal stopped. Green when it finished, yellow when a limit fired —
    /// which is the one distinction somebody scrolling back is looking for.
    pub fn ending(
        &self,
        message: &str,
        ok: bool,
        iterations: u32,
        tokens: i64,
    ) -> Vec<Line<'static>> {
        let role = if ok { Role::Ok } else { Role::Warn };
        let mut out = vec![self.marked(
            self.glyphs.end,
            role,
            Span::styled(message.to_string(), self.palette.bold(role)),
            "",
        )];
        out.push(Line::from(Span::styled(
            format!(
                "  {iterations} calls {} {tokens} tokens (cache-weighted)",
                self.glyphs.sep
            ),
            self.palette.dim(),
        )));
        out
    }

    pub fn note(&self, text: &str) -> Vec<Line<'static>> {
        vec![Line::from(vec![
            Span::styled(format!("{} ", self.glyphs.note), self.palette.dim()),
            Span::styled(text.to_string(), self.palette.dim()),
        ])]
    }

    pub fn warn(&self, text: &str) -> Vec<Line<'static>> {
        vec![self.marked(
            self.glyphs.warn,
            Role::Warn,
            Span::styled(text.to_string(), self.palette.style(Role::Warn)),
            "",
        )]
    }

    pub fn banner(&self, text: &str) -> Vec<Line<'static>> {
        vec![self.marked(
            self.glyphs.banner,
            Role::Err,
            Span::styled(text.to_string(), self.palette.bold(Role::Err)),
            "",
        )]
    }

    /// Assistant prose. No glyph, no colour: it is the answer, and decorating
    /// it would make it look like commentary about itself.
    pub fn prose(&self, text: &str) -> Line<'static> {
        Line::from(Span::styled(
            text.to_string(),
            self.palette.style(Role::Text),
        ))
    }

    /// The record of a question and the answer it got, as one transcript line
    /// — so what was approved reads back the way it was asked.
    pub fn answered(&self, question: &str, answer: &str) -> Vec<Line<'static>> {
        vec![Line::from(vec![
            Span::styled(format!("  {question}"), self.palette.dim()),
            Span::styled(answer.to_string(), self.palette.bold(Role::Accent)),
        ])]
    }

    /// A tool result or a failure detail: indented, capped, and honest about
    /// what it left out.
    fn body(&self, text: &str, role: Role) -> Vec<Line<'static>> {
        let style = if role == Role::Dim {
            self.palette.dim()
        } else {
            self.palette.style(role)
        };
        let mut lines = text.lines();
        let mut out: Vec<Line<'static>> = lines
            .by_ref()
            .take(RESULT_LINES)
            .map(|l| Line::from(Span::styled(format!("  {l}"), style)))
            .collect();
        let rest = lines.count();
        if rest > 0 {
            out.push(Line::from(Span::styled(
                format!("  {} {rest} more lines", self.glyphs.ellipsis),
                self.palette.dim(),
            )));
        }
        out
    }
}

// endregion: The skin

// region: Getting lines out of here
// ---------------------------------------------------------------------------
// Getting lines out of here
//
// Two exits. `plain` throws the styling away, for a pipe. `sgr` turns it into
// escape codes, for the fallback path on a real terminal — the frame does not
// use either, because ratatui writes styled cells itself.
// ---------------------------------------------------------------------------

/// The text of a line, with nothing added. What goes into a file, a pipe or a
/// test.
pub fn plain(line: &Line<'_>) -> String {
    line.spans.iter().map(|s| s.content.as_ref()).collect()
}

/// What the fallback path writes for one line.
///
/// **The one decision that keeps a redirected stream clean.** A palette at
/// [`Level::None`] means either `NO_COLOR` or a destination that is not a
/// terminal, and in both cases the answer is the text and nothing else — not
/// even a bold. `emma … | tee` produces a file somebody reads, and an escape
/// byte in it is a defect however tasteful the attribute was.
///
/// The viewport does not come through here: ratatui writes styled cells, so
/// bold and reverse survive there even when colour does not.
pub fn for_stream(skin: &Skin, line: &Line<'_>) -> String {
    if skin.palette.level == Level::None {
        plain(line)
    } else {
        sgr(line)
    }
}

/// The line with its styling, as escape sequences.
pub fn sgr(line: &Line<'_>) -> String {
    let mut out = String::new();
    for span in &line.spans {
        let codes = codes(span.style);
        if codes.is_empty() {
            out.push_str(&span.content);
        } else {
            out.push_str(&format!("\x1b[{}m{}\x1b[0m", codes.join(";"), span.content));
        }
    }
    out
}

/// SGR parameters for one style. Written out rather than delegated to
/// crossterm, because the fallback path must be able to produce a byte for byte
/// predictable line for a test, and because this is the only place in Emma that
/// needs it.
fn codes(style: Style) -> Vec<String> {
    let mut codes = Vec::new();
    if style.add_modifier.contains(Modifier::BOLD) {
        codes.push("1".to_string());
    }
    if style.add_modifier.contains(Modifier::DIM) {
        codes.push("2".to_string());
    }
    if style.add_modifier.contains(Modifier::REVERSED) {
        codes.push("7".to_string());
    }
    if let Some(fg) = style.fg {
        if let Some(code) = color_code(fg, false) {
            codes.push(code);
        }
    }
    if let Some(bg) = style.bg {
        if let Some(code) = color_code(bg, true) {
            codes.push(code);
        }
    }
    codes
}

fn color_code(c: ratatui::style::Color, bg: bool) -> Option<String> {
    use ratatui::style::Color as C;
    let base = if bg { 10 } else { 0 };
    let named = |n: u32| Some(format!("{}", n + base));
    match c {
        // The user's own colour: say nothing rather than asserting one.
        C::Reset => None,
        C::Black => named(30),
        C::Red => named(31),
        C::Green => named(32),
        C::Yellow => named(33),
        C::Blue => named(34),
        C::Magenta => named(35),
        C::Cyan => named(36),
        C::Gray => named(37),
        C::DarkGray => named(90),
        C::LightRed => named(91),
        C::LightGreen => named(92),
        C::LightYellow => named(93),
        C::LightBlue => named(94),
        C::LightMagenta => named(95),
        C::LightCyan => named(96),
        C::White => named(97),
        C::Indexed(i) => Some(format!("{};5;{i}", 38 + base)),
        C::Rgb(r, g, b) => Some(format!("{};2;{r};{g};{b}", 38 + base)),
    }
}

// endregion: Getting lines out of here

// region: The status line
// ---------------------------------------------------------------------------
// The status line
//
// The one thing on screen that is *live*, which is what makes it the one thing
// that can lie. Everything here is about not lying: a field whose value is not
// known is absent, and a line that does not fit drops fields in a stated order
// rather than being cut mid-number.
// ---------------------------------------------------------------------------

/// What the status line has to say.
///
/// Every field is `Option` for the same reason: a readout that is stale is
/// worse than a readout that is missing. `context` and `spend` are `None` until
/// a model call has actually reported them, and `elapsed` is `None` whenever no
/// goal is running — an elapsed time that is not advancing is a stopped clock
/// somebody will read as a slow one.
#[derive(Debug, Default, Clone)]
pub struct Status {
    pub model: String,
    pub cwd: String,
    pub session: String,
    /// **A snapshot.** The provider's own input count for the most recent call,
    /// and the compaction cap it is measured against. It rises as a
    /// conversation grows and falls when the conversation is compacted.
    pub context: Option<(i64, i64)>,
    /// **A running total.** Everything this goal has been billed for, weighted
    /// by price — cache writes at 1.25, cache reads at 0.1, plus output — and
    /// the budget that ends the goal. It only ever rises.
    pub spend: Option<(i64, i64)>,
    /// How long the running goal has been running.
    pub elapsed: Option<std::time::Duration>,
}

impl Skin {
    /// The status line, fitted to the width it has.
    ///
    /// **Fields are dropped, never truncated mid-value.** A `total 34k/50` that
    /// ran out of room is a number that is wrong rather than absent, and the
    /// whole argument for this line is that everything on it is true. The order
    /// below is the order things go: the session id first because it is the
    /// least useful thing to read at a glance, then the working directory
    /// shortens to its last component, then it goes too.
    pub fn status(&self, width: u16, s: &Status) -> Line<'static> {
        let live = self.live_fields(s);
        for attempt in 0..5 {
            let left = self.identity(s, attempt);
            // Columns, not characters: a working directory with CJK in its name
            // is twice as wide as it is long, and the gap computed from its
            // length pushes the live fields off the end of the row.
            let used: usize = left.iter().chain(live.iter()).map(|sp| sp.width()).sum();
            let gap = usize::from(width).saturating_sub(used);
            if gap >= 1 || attempt == 4 {
                let mut spans = left;
                spans.push(Span::raw(" ".repeat(gap.max(1))));
                spans.extend(live);
                return Line::from(spans);
            }
        }
        unreachable!("the last attempt always returns")
    }

    /// Model, directory, session — the facts that are fixed for the process.
    fn identity(&self, s: &Status, drop: usize) -> Vec<Span<'static>> {
        let mut spans = vec![Span::styled(
            "emma".to_string(),
            self.palette.bold(Role::Accent),
        )];
        let add = |text: String, style: Style, spans: &mut Vec<Span<'static>>| {
            spans.push(Span::styled(
                format!(" {} ", self.glyphs.sep),
                self.palette.dim(),
            ));
            spans.push(Span::styled(text, style));
        };
        if drop < 4 && !s.model.is_empty() {
            add(s.model.clone(), self.palette.style(Role::Info), &mut spans);
        }
        if drop < 3 && !s.cwd.is_empty() {
            let cwd = if drop < 2 {
                s.cwd.clone()
            } else {
                last_component(&s.cwd)
            };
            add(cwd, self.palette.style(Role::Text), &mut spans);
        }
        if drop < 1 && !s.session.is_empty() {
            add(s.session.clone(), self.palette.dim(), &mut spans);
        }
        spans
    }

    /// The live half. Absent fields are absent — see [`Status`].
    ///
    /// **`ctx` and `total`, and the question that renamed the second one.** The
    /// owner read `ctx 73k/120k · spend 103k/500k` and asked whether the two
    /// were the wrong way round, because spend was the larger number. They were
    /// not: the second call of any goal spends more than the first call's
    /// context, and every call after that widens the gap. But a label that
    /// invites the question is the defect, and `spend` invited it — it reads as
    /// a level, like `ctx`, when it is an accumulation.
    ///
    /// So the label names what the number *is* rather than what it is measured
    /// against: `ctx` is how full the window is right now, and `total` is what
    /// the goal has been billed for so far. A total exceeding a level is not a
    /// thing anybody has to reason about. `total` is the same five columns
    /// `spend` was, so the order this line drops fields in is unchanged — see
    /// [`Skin::status`], which drops whole fields rather than cutting a number.
    fn live_fields(&self, s: &Status) -> Vec<Span<'static>> {
        let mut spans: Vec<Span<'static>> = Vec::new();
        let push = |label: &str, value: String, role: Role, spans: &mut Vec<Span<'static>>| {
            if !spans.is_empty() {
                spans.push(Span::styled(
                    format!(" {} ", self.glyphs.sep),
                    self.palette.dim(),
                ));
            }
            spans.push(Span::styled(format!("{label} "), self.palette.dim()));
            spans.push(Span::styled(value, self.palette.style(role)));
        };
        if let Some((used, cap)) = s.context {
            push(
                "ctx",
                format!("{}/{}", human(used), human(cap)),
                pressure(used, cap),
                &mut spans,
            );
        }
        if let Some((used, cap)) = s.spend {
            push(
                "total",
                format!("{}/{}", human(used), human(cap)),
                pressure(used, cap),
                &mut spans,
            );
        }
        if let Some(elapsed) = s.elapsed {
            push("", clock(elapsed), Role::Text, &mut spans);
        }
        spans
    }
}

/// Green with room to spare, yellow past three quarters, red past the cap.
///
/// The cap is a real thing in both cases — [`crate::Budgets::max_context`]
/// triggers compaction and `max_tokens` ends the goal — so the colour is a
/// statement about what is going to happen, not decoration.
fn pressure(used: i64, cap: i64) -> Role {
    if cap <= 0 {
        return Role::Text;
    }
    match used * 4 / cap {
        0..=2 => Role::Ok,
        3 => Role::Warn,
        _ => Role::Err,
    }
}

/// Token counts, short enough for a status line.
fn human(n: i64) -> String {
    match n.abs() {
        0..=9_999 => n.to_string(),
        10_000..=999_999 => format!("{}k", n / 1_000),
        _ => format!("{:.1}M", n as f64 / 1_000_000.0),
    }
}

/// Elapsed time, in the units a person waiting actually reads.
fn clock(d: std::time::Duration) -> String {
    let s = d.as_secs();
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m{:02}s", s / 60, s % 60)
    } else {
        format!("{}h{:02}m", s / 3600, (s % 3600) / 60)
    }
}

/// The last component of a path, for a status line that has run out of room.
fn last_component(path: &str) -> String {
    path.rsplit(['/', '\\'])
        .find(|p| !p.is_empty())
        .unwrap_or(path)
        .to_string()
}

// endregion: The status line

// region: Fitting text
// ---------------------------------------------------------------------------
// Fitting text
//
// **Everything in this region counts display columns, never characters.** A
// terminal cell is a column; `一` occupies two of them, `é` written as `e` plus
// a combining acute occupies one, and both of those are one `char`. Anywhere a
// budget derived from `area.width` was compared against `chars().count()`, a
// line of CJK overran its box by its own length and the cell after it was
// somebody else's.
//
// The measurement is ratatui's own — `Span::width`, which is what `Layout` and
// `Buffer` measure with — rather than a second opinion from a crate imported
// beside it. Two width tables that disagree is the same class of bug as two
// crossterm versions that disagree about a `KeyEvent`, and the manifest already
// argues that one.
//
// The honest limit: unicode-width sums per character, so a ZWJ emoji sequence
// is measured as the sum of its parts while a terminal draws one glyph. Nothing
// here can fix that — ratatui will lay it out by the same wrong number, so at
// least Emma and its renderer are wrong together, which is the property that
// keeps a box from being overrun by its own contents.
// ---------------------------------------------------------------------------

/// How many terminal columns a string occupies.
pub fn cols(text: &str) -> usize {
    Span::raw(text).width()
}

/// How many columns the first `chars` characters occupy.
///
/// The bridge between the two coordinate systems that meet at the input box:
/// [`super::input::Editor`] counts a cursor in characters, because that is what
/// Left and Right move by, and the terminal wants a column.
pub fn cols_upto(text: &str, chars: usize) -> usize {
    text.chars().take(chars).map(char_cols).sum()
}

/// One character's columns, without allocating to ask.
fn char_cols(c: char) -> usize {
    let mut buf = [0u8; 4];
    cols(c.encode_utf8(&mut buf))
}

/// Cut a string to a column budget, marking the cut.
///
/// **A wide character is never cut in half.** Half of `一` is not a narrow
/// glyph, it is a cell the terminal fills with something of its own choosing —
/// so when the next character would cross the budget the cut happens before it,
/// and the result is one column short rather than one column over. Short is
/// recoverable; over writes into the next widget.
pub fn fit(text: &str, budget: usize, ellipsis: &str) -> String {
    if cols(text) <= budget {
        return text.to_string();
    }
    let keep = budget.saturating_sub(cols(ellipsis));
    let mut out = String::new();
    let mut used = 0;
    for c in text.chars() {
        let w = char_cols(c);
        if used + w > keep {
            break;
        }
        used += w;
        out.push(c);
    }
    out.push_str(ellipsis);
    out
}

/// How many screen rows a line takes at this width, wrapping.
///
/// [`Line::width`] measures display columns rather than characters, so a CJK
/// glyph counts as the two columns it occupies. That matters here because the
/// number is handed to `insert_before`, which needs to know in advance how much
/// room to make: guess low and the last row of a wrapped line lands on top of
/// the viewport.
pub fn rows_used(line: &Line<'_>, width: u16) -> u16 {
    let width = usize::from(width.max(1));
    (line.width().max(1)).div_ceil(width).max(1) as u16
}

/// Break a styled line into rows of at most `width` columns, keeping the
/// styling.
///
/// The terminal used to do this: inline output is handed to `insert_before` and
/// wrapped by whatever the window is. [`super::transcript`] owns a buffer the
/// terminal never sees, so the wrap has to happen here — and it has to happen
/// on the *styled* line, because splitting the plain text and re-styling it is
/// how a diff's `+` ends up green on one row and not on the next.
///
/// A single character wider than the whole width gets a row to itself rather
/// than an infinite loop; that is a two-column window, and the fallback gate
/// refuses below twenty-four.
pub fn wrap_line(line: &Line<'_>, width: u16) -> Vec<Line<'static>> {
    let width = usize::from(width.max(1));
    if line.width() <= width {
        return vec![owned(line)];
    }
    let mut rows: Vec<Line<'static>> = Vec::new();
    let mut row: Vec<Span<'static>> = Vec::new();
    let mut used = 0;
    for span in &line.spans {
        let mut text = String::new();
        for c in span.content.chars() {
            let w = char_cols(c);
            if used + w > width && !(row.is_empty() && text.is_empty()) {
                if !text.is_empty() {
                    row.push(Span::styled(std::mem::take(&mut text), span.style));
                }
                rows.push(Line::from(std::mem::take(&mut row)));
                used = 0;
            }
            text.push(c);
            used += w;
        }
        if !text.is_empty() {
            row.push(Span::styled(text, span.style));
        }
    }
    if !row.is_empty() {
        rows.push(Line::from(row));
    }
    rows
}

/// A borrowed line, owned. `Line::to_owned` would clone the borrow, not the
/// data.
pub fn owned(line: &Line<'_>) -> Line<'static> {
    Line {
        style: line.style,
        alignment: line.alignment,
        spans: line
            .spans
            .iter()
            .map(|s| Span::styled(s.content.to_string(), s.style))
            .collect(),
    }
}

// endregion: Fitting text

#[cfg(test)]
mod tests {
    use super::*;

    fn skin(level: Level) -> Skin {
        Skin::new(Palette::new(level), UNICODE)
    }

    /// The defect this whole vocabulary replaces: every tool line was `●`,
    /// whatever had happened.
    ///
    /// Written as "no two of these share a glyph" rather than as six
    /// assertions on six literals, because the property is the distinctness
    /// and a test on the literals would pass a change that made two of them
    /// identical.
    #[test]
    fn a_started_a_succeeded_a_failed_a_refused_and_a_blocked_call_all_look_different() {
        let s = skin(Level::Truecolor);
        let first = |lines: Vec<Line<'static>>| {
            let line = &lines[0];
            let span = line.spans[0].clone();
            (span.content.to_string(), span.style)
        };
        let events = [
            first(s.tool_started("Bash", "cargo test")),
            first(s.tool_failed("Bash", "exit 1")),
            first(s.tool_refused("Bash", "you declined it; the model was told")),
            first(s.tool_blocked("Bash", "no shell in this repo")),
            first(s.kick(1, 3)),
            first(s.ending("goal complete", true, 4, 100)),
            first(s.goal("port the middleware")),
        ];
        for (i, (glyph, _)) in events.iter().enumerate() {
            for (j, (other, _)) in events.iter().enumerate() {
                assert!(
                    i == j || glyph != other,
                    "two kinds of event share the glyph {glyph:?}"
                );
            }
        }
        // And the two that a user must not confuse — "you said no" against
        // "policy said no" — differ in colour as well as in glyph, because one
        // of them is a decision the user can revisit and the other is not.
        let refused = first(s.tool_refused("Bash", "you declined it; the model was told"))
            .1
            .fg;
        let blocked = first(s.tool_blocked("Bash", "x")).1.fg;
        assert_ne!(refused, blocked);
    }

    #[test]
    fn a_successful_result_is_dim_and_a_failure_is_red() {
        let s = skin(Level::Truecolor);
        let ok = s.tool_ok("all good", false, None);
        assert!(ok[0].spans[0].style.add_modifier.contains(Modifier::DIM));
        let bad = s.tool_failed("Bash", "boom");
        assert_eq!(bad[1].spans[0].style.fg, Some(s.palette.color(Role::Err)));
    }

    /// Two different kinds of "there is more than this", kept apart on purpose.
    #[test]
    fn a_truncated_tool_and_a_long_result_say_different_things() {
        let s = skin(Level::Truecolor);
        let long = s.tool_ok(&"line\n".repeat(30), false, None);
        let text: String = long.iter().map(plain).collect::<Vec<_>>().join("\n");
        assert!(text.contains("22 more lines"), "{text}");
        // The screen left some out. The tool itself did not stop early, so
        // nothing may claim it did.
        assert!(!text.contains("truncated"), "{text}");

        let cut = s.tool_ok("a\nb", true, None);
        let text: String = cut.iter().map(plain).collect::<Vec<_>>().join("\n");
        assert!(text.contains("truncated"), "{text}");
    }

    /// The line the owner actually saw, and the fix. A tool that named its cap
    /// gets that sentence on screen verbatim; one that did not says so rather
    /// than implying the reader has been told which limit bound.
    #[test]
    fn a_stated_reason_reaches_the_screen_whole() {
        let s = skin(Level::Truecolor);
        let reason = "50 of 70 links shown, 20 dropped by max_links=50; \
                      re-read with max_links=70 for the rest";
        let cut = s.tool_ok("a\nb", true, Some(reason));
        let text: String = cut.iter().map(plain).collect::<Vec<_>>().join("\n");
        assert!(
            text.contains(reason),
            "the reason was dropped or reworded: {text}"
        );

        let vague = s.tool_ok("a\nb", true, None);
        let text: String = vague.iter().map(plain).collect::<Vec<_>>().join("\n");
        assert!(text.contains("did not say by which limit"), "{text}");
    }

    /// A skin built with a theme is the same skin, and it is still `Copy` —
    /// which is what lets `Term`, `Frame` and `View` keep storing one by value.
    /// A theme that owned a `String` would end that and cascade through every
    /// signature in this module, so the swap is a whole-value replacement of
    /// fixed-size data or it is a much larger change than stage 1.
    #[test]
    fn a_skin_carrying_a_theme_is_still_copy_and_still_the_same_skin() {
        fn assert_copy<T: Copy>() {}
        assert_copy::<Skin>();
        assert_copy::<Glyphs>();

        for level in [Level::None, Level::Ansi16, Level::Ansi256, Level::Truecolor] {
            let swapped = Skin::with_theme(level, UNICODE, super::super::theme::BUILTIN);
            let built = Skin::new(Palette::new(level), UNICODE);
            assert_eq!(swapped.palette.level, built.palette.level);
            assert_eq!(swapped.glyphs, built.glyphs);
            for role in [Role::Text, Role::Accent, Role::Err, Role::Ground] {
                assert_eq!(
                    swapped.palette.color(role),
                    built.palette.color(role),
                    "{role:?} at {level:?}"
                );
            }
        }
    }

    #[test]
    fn assistant_prose_is_left_undecorated() {
        // The answer is the thing the user came for. A glyph in front of it
        // would file it as commentary about itself.
        let s = skin(Level::Truecolor);
        let line = s.prose("Ported the middleware.");
        assert_eq!(plain(&line), "Ported the middleware.");
        assert_eq!(line.spans[0].style.fg, Some(ratatui::style::Color::Reset));
    }

    // -----------------------------------------------------------------------
    // Getting out
    // -----------------------------------------------------------------------

    /// The guarantee that makes `emma … | tee` safe: with no colour — which is
    /// what a pipe and what `NO_COLOR` both produce — nothing this file writes
    /// contains an escape byte. Not a colour, and not a bold either.
    #[test]
    fn with_no_colour_not_one_escape_byte_is_emitted() {
        let s = skin(Level::None);
        let everything = [
            s.goal("g"),
            s.tool_started("Bash", "ls"),
            s.tool_ok("out", true, Some("50 of 70 links shown")),
            s.tool_failed("Bash", "boom"),
            s.tool_refused("Bash", "you declined it; the model was told"),
            s.tool_blocked("Bash", "policy"),
            s.kick(1, 3),
            s.ending("done", true, 1, 2),
            s.note("n"),
            s.warn("w"),
            s.banner("b"),
            s.answered("allow? ", "y"),
        ];
        for line in everything.iter().flatten() {
            let out = for_stream(&s, line);
            assert!(
                !out.contains('\x1b'),
                "an escape sequence reached a redirected stream: {out:?}"
            );
            assert_eq!(out, plain(line));
        }
    }

    #[test]
    fn colour_becomes_escape_codes_only_when_there_is_colour_to_write() {
        let s = skin(Level::Ansi16);
        let out = for_stream(&s, &s.tool_failed("Bash", "x")[0]);
        assert!(out.contains("\x1b["), "{out:?}");
        // Bright red, bold, and reset afterwards — never left set.
        assert!(out.contains("91"), "{out:?}");
        assert!(out.ends_with("\x1b[0m"), "{out:?}");
    }

    #[test]
    fn a_truecolor_span_is_written_as_24_bit() {
        let s = skin(Level::Truecolor);
        assert!(for_stream(&s, &s.warn("careful")[0]).contains("38;2;250;189;47"));
    }

    // -----------------------------------------------------------------------
    // The status line
    // -----------------------------------------------------------------------

    fn status() -> Status {
        Status {
            model: "claude-opus-4".into(),
            cwd: "C:\\src\\emma".into(),
            session: "sess-123".into(),
            context: Some((12_000, 120_000)),
            spend: Some((34_000, 500_000)),
            elapsed: Some(std::time::Duration::from_secs(72)),
        }
    }

    #[test]
    fn the_status_line_carries_the_run_and_what_it_has_spent() {
        let out = plain(&skin(Level::Truecolor).status(120, &status()));
        for expected in ["emma", "claude-opus-4", "C:\\src\\emma", "sess-123"] {
            assert!(out.contains(expected), "{expected} is missing: {out}");
        }
        assert!(out.contains("ctx 12k/120k"), "{out}");
        assert!(out.contains("total 34k/500k"), "{out}");
        assert!(out.contains("1m12s"), "{out}");
    }

    /// **The two meters are different kinds of number and the labels say so.**
    ///
    /// The failure this pins is the one that was reported: a reader seeing the
    /// second number larger than the first and concluding the line was
    /// inverted. `spend` reads as a level; `total` cannot. Written against the
    /// realistic case — a goal several calls in, where the running total is
    /// genuinely larger than the current context — because that is the exact
    /// screen that produced the question.
    #[test]
    fn the_cumulative_meter_is_not_labelled_like_the_snapshot_one() {
        let s = Status {
            context: Some((73_000, 120_000)),
            spend: Some((103_000, 500_000)),
            ..status()
        };
        let out = plain(&skin(Level::Truecolor).status(120, &s));
        assert!(out.contains("ctx 73k/120k"), "{out}");
        assert!(out.contains("total 103k/500k"), "{out}");
        // The word that invited "is that inverted?" is gone, and nothing
        // replaced it with a longer one: the label is the same width, so the
        // narrow-window drop order below is untouched.
        assert!(!out.contains("spend"), "{out}");
    }

    /// A field nobody can keep true is not shown at all.
    ///
    /// This is the rule the previous design followed by leaving spend off the
    /// screen entirely, and it is the reason the line is worth having: every
    /// number on it was measured, and the ones that were not are absent rather
    /// than zero.
    #[test]
    fn a_field_with_no_measurement_behind_it_is_absent_not_zero() {
        let s = Status {
            context: None,
            spend: None,
            elapsed: None,
            ..status()
        };
        let out = plain(&skin(Level::Truecolor).status(120, &s));
        assert!(!out.contains("ctx"), "{out}");
        assert!(!out.contains("spend"), "{out}");
        // The fixed facts are still there — those are true whatever happens.
        assert!(out.contains("claude-opus-4"), "{out}");
    }

    /// Narrow windows drop whole fields rather than cutting a number in half.
    #[test]
    fn a_narrow_window_drops_fields_from_the_least_useful_end() {
        let skin = skin(Level::Truecolor);
        let wide = plain(&skin.status(120, &status()));
        assert!(wide.contains("sess-123"));

        let narrow = plain(&skin.status(64, &status()));
        assert!(!narrow.contains("sess-123"), "{narrow}");
        // The live half survives longest: it is the only part that changes.
        assert!(narrow.contains("total 34k/500k"), "{narrow}");

        let tiny = plain(&skin.status(34, &status()));
        assert!(!tiny.contains("C:\\src"), "{tiny}");
        assert!(tiny.contains("ctx"), "{tiny}");
    }

    #[test]
    fn a_meter_near_its_cap_changes_colour() {
        assert_eq!(pressure(10, 100), Role::Ok);
        assert_eq!(pressure(80, 100), Role::Warn);
        assert_eq!(pressure(120, 100), Role::Err);
        // A cap of zero is "no cap", not a division by zero.
        assert_eq!(pressure(5, 0), Role::Text);
    }

    #[test]
    fn numbers_and_clocks_are_written_the_way_a_person_reads_them() {
        assert_eq!(human(999), "999");
        assert_eq!(human(12_345), "12k");
        assert_eq!(human(1_500_000), "1.5M");
        assert_eq!(clock(std::time::Duration::from_secs(9)), "9s");
        assert_eq!(clock(std::time::Duration::from_secs(605)), "10m05s");
        assert_eq!(clock(std::time::Duration::from_secs(7_300)), "2h01m");
    }

    #[test]
    fn a_wrapped_line_is_counted_in_the_rows_it_will_take() {
        let s = skin(Level::None);
        assert_eq!(rows_used(&s.prose(""), 20), 1);
        assert_eq!(rows_used(&s.prose(&"x".repeat(20)), 20), 1);
        assert_eq!(rows_used(&s.prose(&"x".repeat(21)), 20), 2);
        // A zero width would divide by zero, and a resize can report anything.
        assert!(rows_used(&s.prose("xx"), 0) >= 1);
    }

    #[test]
    fn a_short_line_is_left_alone_and_a_long_one_is_cut() {
        assert_eq!(fit("emma", 40, "…"), "emma");
        let cut = fit(&"a".repeat(200), 10, "…");
        assert_eq!(cut.chars().count(), 10);
        assert!(cut.ends_with('…'));
    }

    // -----------------------------------------------------------------------
    // Display columns
    //
    // The budgets these functions take come from `area.width`, which is cells.
    // Every test below is a string whose character count and column count
    // differ, because a string where they agree cannot tell the two apart —
    // and every one of these passed before the fix.
    // -----------------------------------------------------------------------

    /// `一` is two columns and one character; a combining acute is zero columns
    /// and one character. Both were counted as one.
    #[test]
    fn width_is_measured_in_columns_not_characters() {
        assert_eq!(cols("emma"), 4);
        assert_eq!("一二三".chars().count(), 3);
        assert_eq!(cols("一二三"), 6);
        // `e` + U+0301. Two characters, one cell.
        assert_eq!("e\u{301}".chars().count(), 2);
        assert_eq!(cols("e\u{301}"), 1);
    }

    /// **The overrun.** Eight CJK characters are sixteen columns, and a budget
    /// of twelve used to let all eight through untouched — sixteen cells
    /// written into a twelve-cell box, four of them somebody else's.
    #[test]
    fn a_wide_string_is_cut_to_the_columns_it_has_rather_than_the_characters() {
        let text = "一".repeat(8);
        let cut = fit(&text, 12, "…");
        assert!(
            cols(&cut) <= 12,
            "{cut:?} is {} columns in a 12-column budget",
            cols(&cut)
        );
        assert!(cut.ends_with('…'));
        // Short rather than over: the last whole glyph that fits, and never
        // half of one.
        assert_eq!(cut, "一一一一一…");
    }

    /// A cut that landed inside a wide character would give the terminal half a
    /// glyph, which it fills with whatever it likes.
    #[test]
    fn a_cut_never_lands_inside_a_wide_character() {
        for budget in 1..=20 {
            let cut = fit(&"一".repeat(10), budget, "…");
            assert!(
                cols(&cut) <= budget,
                "budget {budget} produced {} columns: {cut:?}",
                cols(&cut)
            );
        }
    }

    /// The bridge the input box crosses: a character index becomes a column.
    #[test]
    fn a_character_index_becomes_the_column_the_cursor_belongs_on() {
        assert_eq!(cols_upto("emma", 2), 2);
        // Two CJK characters typed, cursor after them: column four, not two.
        assert_eq!(cols_upto("一二三", 2), 4);
        assert_eq!(cols_upto("一二三", 0), 0);
        // Past the end is the whole string rather than a panic — a cursor can
        // be stale by one keystroke while a repaint is in flight.
        assert_eq!(cols_upto("一二三", 99), 6);
    }

    /// **A combining mark costs the cursor no column**, and until this test
    /// nothing said so.
    ///
    /// Found by mutation. Changing [`char_cols`] to
    /// `cols(c.encode_utf8(&mut buf)).max(1)` — one character, one line — left
    /// the entire `emma` lib suite green: 527 passed, 0 failed. The existing
    /// column tests are all CJK, where every character is *wider* than one
    /// cell, so a floor of one is invisible to every one of them. The other
    /// direction, a character *narrower* than one cell, had no test at all.
    ///
    /// It is [`cols_upto`] that pays for it, because that is the bridge the
    /// input box crosses: [`super::input::Editor`] counts the cursor in
    /// characters and the terminal wants a column. `é` typed as `e` + U+0301 is
    /// two characters and one cell, so with a floor of one the cursor sits a
    /// column to the right of the letter it is supposed to be after — and the
    /// error accumulates, one column per mark, which is what makes it a
    /// Vietnamese or Hebrew user's whole line rather than one glyph.
    ///
    /// **What this establishes, and what it does not.** It establishes that
    /// Emma's arithmetic and ratatui's agree, because both go through
    /// `Span::width`. It does not establish that a terminal draws `e` + U+0301
    /// in one cell — some do not, some draw the mark on the following cell, and
    /// no cell buffer can tell. The module doc above already names the same
    /// limit for ZWJ sequences: Emma is wrong together with its renderer, which
    /// keeps a box from being overrun, not from looking wrong.
    #[test]
    fn a_combining_mark_costs_the_cursor_no_column() {
        // The bridge, at every cursor position across the mark.
        let text = "e\u{301}mma";
        assert_eq!(text.chars().count(), 5);
        assert_eq!(cols(text), 4);
        assert_eq!(cols_upto(text, 0), 0);
        assert_eq!(cols_upto(text, 1), 1, "the base letter is one column");
        assert_eq!(
            cols_upto(text, 2),
            1,
            "the combining acute was given a column of its own"
        );
        assert_eq!(cols_upto(text, 5), 4);
        // Several marks in a row, because the error is cumulative and one mark
        // is within rounding of being right by accident.
        assert_eq!(cols_upto("a\u{301}\u{302}\u{303}", 4), 1);

        // A zero-width character is not the only narrow case, and the control
        // that stops a floor of zero from passing: an ordinary letter still
        // costs one, and a CJK glyph still costs two.
        assert_eq!(cols_upto("abc", 3), 3);
        assert_eq!(cols_upto("一", 1), 2);

        // And `fit` counts the same way, so a marked-up name is not cut short
        // by the marks on it.
        assert_eq!(fit("e\u{301}mma", 4, "…"), "e\u{301}mma");
    }

    /// **`fit` never overruns at any budget either glyph set actually
    /// reaches** — and it has a precondition, which is written down here
    /// because it is not written down anywhere else.
    ///
    /// The existing wide-character sweep starts at budget 1 and only ever
    /// passes the one-column `…`. The ASCII glyph set's ellipsis is `...`,
    /// three columns, and at a budget below that `fit` returns the ellipsis
    /// alone and is **wider than the budget it was given**. Measured, not
    /// inferred:
    ///
    /// ```text
    /// fit("abcdef", 0, "…")   -> "…"    1 column in a 0-column budget
    /// fit("abcdef", 2, "...") -> "..."  3 columns in a 2-column budget
    /// ```
    ///
    /// [`super::sidebar::clipped`] already knows this and guards it with a
    /// `budget < cols(ellipsis)` branch of its own; `view.rs`, `statusbar.rs`
    /// and `app.rs` call `fit` directly and do not. That is a defect in `fit`'s
    /// contract rather than in this test, and it is reported rather than
    /// pinned: a test asserting the overrun would make the fix red.
    ///
    /// So this asserts the range the callers can rely on — every budget from
    /// `cols(ellipsis)` up — over both glyph sets and over text that is narrow,
    /// wide, and mixed. Mutating `keep` to `budget` in [`fit`] turns it red.
    #[test]
    fn a_cut_never_overruns_at_any_budget_the_glyph_sets_actually_use() {
        for ellipsis in [UNICODE.ellipsis, ASCII.ellipsis] {
            let floor = cols(ellipsis);
            assert!(floor >= 1, "an ellipsis of no width proves nothing");
            for text in ["x".repeat(40), "一".repeat(20), "a一b二c三".repeat(6)] {
                for budget in floor..=24 {
                    let cut = fit(&text, budget, ellipsis);
                    assert!(
                        cols(&cut) <= budget,
                        "fit({text:?}, {budget}, {ellipsis:?}) is {} columns",
                        cols(&cut)
                    );
                    // Cut, and said so. A silent cut is the failure this
                    // function exists to avoid, and an implementation that
                    // returned an empty string would satisfy the bound above.
                    assert!(cut.ends_with(ellipsis), "the cut was not marked: {cut:?}");
                }
            }
            // **The control: text that fits is returned untouched.** Without
            // it, a `fit` that marked everything would pass every assertion
            // above, and every short name on screen would grow a lie.
            let short = "emma";
            assert_eq!(fit(short, 40, ellipsis), short);
            assert_eq!(fit("一二", 4, ellipsis), "一二");
        }
    }

    /// The status row is right-aligned by arithmetic, and the arithmetic used
    /// to be in characters: a CJK directory name pushed the live fields off the
    /// end of the row it was supposed to fit inside.
    #[test]
    fn a_wide_directory_name_does_not_push_the_status_off_the_row() {
        let skin = skin(Level::Truecolor);
        let mut s = Status {
            cwd: "一".repeat(12),
            model: "claude".into(),
            ..Status::default()
        };
        s.context = Some((30_000, 100_000));
        let line = skin.status(60, &s);
        assert!(
            line.width() <= 60,
            "the status row is {} columns wide in 60: {:?}",
            line.width(),
            plain(&line)
        );
    }

    // -----------------------------------------------------------------------
    // Wrapping styled lines
    // -----------------------------------------------------------------------

    #[test]
    fn a_wrapped_line_keeps_every_character_and_its_styling() {
        let skin = skin(Level::Truecolor);
        let line = Line::from(vec![
            Span::styled("abcdef".to_string(), skin.palette.style(Role::Ok)),
            Span::styled("ghijkl".to_string(), skin.palette.style(Role::Err)),
        ]);
        let rows = wrap_line(&line, 4);
        assert_eq!(
            rows.iter().map(plain).collect::<Vec<_>>(),
            ["abcd", "efgh", "ijkl"]
        );
        // The `e` and `f` are still green and the `g` and `h` still red — the
        // reason this splits the styled line rather than the plain text.
        let second = &rows[1];
        assert_eq!(second.spans[0].content, "ef");
        assert_eq!(second.spans[0].style, skin.palette.style(Role::Ok));
        assert_eq!(second.spans[1].style, skin.palette.style(Role::Err));
    }

    #[test]
    fn a_wrap_breaks_between_wide_characters_rather_than_through_one() {
        let line = Line::from(Span::raw("一二三四五".to_string()));
        let rows = wrap_line(&line, 5);
        // Five columns holds two glyphs, not two and a half.
        assert_eq!(
            rows.iter().map(plain).collect::<Vec<_>>(),
            ["一二", "三四", "五"]
        );
        for row in &rows {
            assert!(row.width() <= 5, "{:?}", plain(row));
        }
    }

    /// A window narrower than one glyph is not a reason to loop forever.
    #[test]
    fn a_character_wider_than_the_window_still_gets_a_row() {
        let rows = wrap_line(&Line::from(Span::raw("一一".to_string())), 1);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows.iter().map(plain).collect::<Vec<_>>(), ["一", "一"]);
    }
}
