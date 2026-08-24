//! The model writes markdown. This is as much of it as a terminal transcript
//! can carry honestly, and nothing more.
//!
//! # Why a subset rather than a markdown crate
//!
//! `pulldown-cmark` is the obvious answer and it is the wrong shape for this
//! job twice over. It is an *event* parser over a whole document — it wants the
//! text before it can tell you what the text was — and prose here arrives in
//! fragments, a few characters at a time, with the screen updating as it goes.
//! And it parses the whole of CommonMark, including link reference definitions,
//! HTML blocks and setext headings, none of which a terminal can render as
//! anything other than the characters they were written with. What is left after
//! the parts a terminal cannot show is a handful of line shapes, and those are
//! cheaper to recognise here than to translate from somebody else's event
//! stream. So: no dependency, and no `Cargo.toml` change to justify.
//!
//! # The boundary, and why it is the newline
//!
//! **A line is styled the moment its newline arrives, and never before.** Every
//! block this file recognises is decided by the *start* of a line — `#`, `-`,
//! `1.`, ` ``` ` — so one complete line is all the context any of them need.
//! The one piece of state that crosses lines is whether a fence is open, and a
//! fence opens and closes on lines of its own, so that is a `bool`'s worth of
//! memory rather than a parse tree.
//!
//! The alternative — buffer a paragraph, or a document, and render it when it
//! is complete — was rejected on feel. Emma streams because forty seconds of
//! blank terminal is indistinguishable from a hang; a renderer that waits for a
//! blank line before it prints anything reintroduces exactly that pause, at the
//! end of every paragraph, in the one place the user is actually reading.
//!
//! The cost is stated rather than hidden: **setext headings** (`Title` followed
//! by `=====`) cannot be recognised, because by the time the `=====` arrives the
//! title has already been printed. They degrade to a paragraph and a rule, which
//! is what they look like. **Tables** are not aligned, because column widths are
//! a property of the whole table. Their rows are printed as written, which is
//! legible and true.
//!
//! # What must survive being scrollback
//!
//! Every line this file produces is written above the viewport and is final: the
//! terminal owns it, wraps nothing further, and a mouse can select it. Three
//! rules follow from that and each one is a test.
//!
//! **Nothing is reflowed that changes meaning.** Prose is wrapped at word
//! boundaries to the terminal's width — better than the terminal's own wrap,
//! which breaks mid-word — but a line inside a fenced code block is *never*
//! wrapped at a word. A code line too wide for the window is continued at the
//! exact column the window ends, which is what the terminal itself would have
//! done, and joining the rows back together reproduces the source byte for byte.
//!
//! **Nothing is inserted into what a reader copies.** No gutter down the side of
//! a code block, no `│` in front of quoted text, no indent added to prose. The
//! markers this file *removes* — the `#` of a heading, the `**` of a bold run,
//! the backticks around a code span — are removed because that is what rendering
//! them means; nothing takes their place.
//!
//! **Every row fits the width it was given.** `insert_before` is told in advance
//! how many rows to make room for, and that count is `ceil(width / columns)`.
//! A row wider than the window would take a row more than was reserved and its
//! tail would land on top of the viewport. So the wrap here is not decoration —
//! it is what keeps the arithmetic in `frame.rs` true.
//!
//! # Degrading
//!
//! Colour comes from [`Role`] and nowhere else, so a 16-colour terminal gets the
//! named ANSI colours and a colourless one gets bold and dim. Glyphs come from
//! [`Skin::glyphs`](super::render::Skin), so an ASCII console gets `-` where a
//! UTF-8 one gets `•` and `─`. Anything this file does not recognise — a table,
//! an image, an HTML span, `*italics*`, a footnote — is emitted as the text it
//! was written as. **Content is never swallowed**: the worst case is plain.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use super::palette::Role;
use super::render::Skin;

// region: The renderer
// ---------------------------------------------------------------------------
// The renderer
//
// One line in, the rows it becomes out. The only field is the open fence, which
// is the only thing about a line that the previous lines decide.
// ---------------------------------------------------------------------------

/// The state a stream of markdown needs, which is one open fence.
#[derive(Debug, Default, Clone)]
pub struct Markdown {
    /// The fence character and how many of it opened the block. CommonMark
    /// closes a fence only with the same character and at least as many of
    /// them, which is what lets a ` ``` ` block contain a `~~~` line.
    fence: Option<(char, usize)>,
}

impl Markdown {
    pub fn new() -> Self {
        Self::default()
    }

    /// Forget any open fence.
    ///
    /// Called when the assistant stops talking. A model that opens a fence and
    /// never closes it would otherwise leave the *next* answer rendered as code
    /// — one malformed reply poisoning every reply after it, which is a worse
    /// failure than the malformed one.
    pub fn reset(&mut self) {
        self.fence = None;
    }

    /// True while a fenced code block is open.
    pub fn in_code(&self) -> bool {
        self.fence.is_some()
    }

    /// One complete source line, as the rows it occupies at this width.
    ///
    /// Every returned line fits `width` display columns. See the module doc for
    /// why that is load-bearing rather than tidy.
    pub fn line(&mut self, raw: &str, width: u16, skin: &Skin) -> Vec<Line<'static>> {
        let width = usize::from(width.max(1));
        let raw = sanitise(raw);

        if let Some((ch, len)) = self.fence {
            if closing_fence(&raw, ch, len) {
                self.fence = None;
                return vec![rule(skin, width, "")];
            }
            return code_rows(&raw, width, skin);
        }
        if let Some((ch, len, info)) = opening_fence(&raw) {
            self.fence = Some((ch, len));
            return vec![rule(skin, width, &info)];
        }
        if raw.trim().is_empty() {
            return vec![Line::default()];
        }
        if thematic_break(&raw) {
            return vec![rule(skin, width, "")];
        }
        if let Some((level, text)) = heading(&raw) {
            let role = match level {
                1 => Role::Accent,
                2 => Role::Info,
                _ => Role::Text,
            };
            let spans = inline(&text, skin, skin.palette.bold(role));
            return flow(Vec::new(), spans, width, 0);
        }
        if let Some((indent, marker, rest)) = list_item(&raw) {
            // The marker keeps its own column and the text hangs under itself,
            // which is the whole of "markers aligned": a wrapped bullet reads as
            // one item rather than as two.
            let shown = match marker.chars().next() {
                Some('-' | '*' | '+') => skin.glyphs.bullet.to_string(),
                _ => marker.clone(),
            };
            let lead = vec![
                Span::raw(" ".repeat(indent)),
                Span::styled(shown.clone(), skin.palette.bold(Role::Accent)),
                Span::raw(" ".to_string()),
            ];
            let hang = indent + cols(&shown) + 1;
            let spans = inline(&rest, skin, skin.palette.style(Role::Text));
            return flow(lead, spans, width, hang);
        }
        if let Some(rest) = quote(&raw) {
            // The `>` is kept rather than replaced: swapping it for a `│` would
            // put a character into the copy that the model never wrote.
            // `"> "` rather than `">"`: the space is the source's own, so the
            // marker column costs the copy nothing, and the text lines up under
            // itself when it wraps.
            let lead = vec![Span::styled("> ".to_string(), skin.palette.dim())];
            let spans = inline(&rest, skin, skin.palette.dim());
            return flow(lead, spans, width, 2);
        }
        let spans = inline(&raw, skin, skin.palette.style(Role::Text));
        flow(Vec::new(), spans, width, 0)
    }
}

// endregion: The renderer

// region: Which line is this
// ---------------------------------------------------------------------------
// Which line is this
//
// Recognisers, one per shape, each answering from the line alone. They are
// deliberately narrow: a shape that is *nearly* a heading is a paragraph, and a
// paragraph is always renderable.
// ---------------------------------------------------------------------------

/// `# Heading` through `###### Heading`, and the text after the hashes.
///
/// The space is required, so `#hashtag` and `#1` are prose — which is what a
/// model writing about an issue number meant.
fn heading(line: &str) -> Option<(usize, String)> {
    let trimmed = line.trim_start();
    if !trimmed.starts_with('#') {
        return None;
    }
    let level = trimmed.chars().take_while(|c| *c == '#').count();
    if !(1..=6).contains(&level) {
        return None;
    }
    let rest = &trimmed[level..];
    if !rest.starts_with(' ') {
        return None;
    }
    // Closing hashes are decoration in CommonMark and are dropped with them.
    Some((
        level,
        rest.trim_start()
            .trim_end()
            .trim_end_matches('#')
            .trim_end()
            .to_string(),
    ))
}

/// `- item`, `* item`, `+ item`, `1. item`, `2) item`: the indent, the marker
/// as written, and the rest.
fn list_item(line: &str) -> Option<(usize, String, String)> {
    let indent = line.len() - line.trim_start().len();
    // Only spaces indent a list; a deeply indented line is somebody's code
    // block and is left alone.
    if indent > 12 {
        return None;
    }
    let rest = line.trim_start();
    let mut chars = rest.chars();
    let first = chars.next()?;
    if matches!(first, '-' | '*' | '+') {
        let after = &rest[first.len_utf8()..];
        let text = after.strip_prefix(' ')?;
        return Some((indent, first.to_string(), text.trim_start().to_string()));
    }
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() || digits.len() > 9 {
        return None;
    }
    let after = &rest[digits.len()..];
    let delim = after.chars().next()?;
    if delim != '.' && delim != ')' {
        return None;
    }
    let text = after[1..].strip_prefix(' ')?;
    Some((
        indent,
        format!("{digits}{delim}"),
        text.trim_start().to_string(),
    ))
}

/// `> quoted`, and what was quoted.
fn quote(line: &str) -> Option<String> {
    let rest = line.trim_start().strip_prefix('>')?;
    Some(rest.strip_prefix(' ').unwrap_or(rest).to_string())
}

/// `---`, `***`, `___`: three or more of one mark and nothing else.
fn thematic_break(line: &str) -> bool {
    let t = line.trim();
    let Some(first) = t.chars().next() else {
        return false;
    };
    matches!(first, '-' | '*' | '_') && t.len() >= 3 && t.chars().all(|c| c == first)
}

/// The character and length of an opening fence, and its info string.
fn opening_fence(line: &str) -> Option<(char, usize, String)> {
    let t = line.trim_start();
    let ch = t.chars().next().filter(|c| *c == '`' || *c == '~')?;
    let len = t.chars().take_while(|c| *c == ch).count();
    if len < 3 {
        return None;
    }
    let info = t[len..].trim().to_string();
    // A backtick fence's info string may not contain a backtick — that rule is
    // what stops `` `a` and `b` `` in a sentence from opening a code block.
    if ch == '`' && info.contains('`') {
        return None;
    }
    Some((ch, len, info))
}

/// Whether this line closes the fence that is open: same character, at least as
/// many, and nothing else on the line.
fn closing_fence(line: &str, ch: char, len: usize) -> bool {
    let t = line.trim();
    t.len() >= len && t.chars().all(|c| c == ch)
}

// endregion: Which line is this

// region: Inline spans
// ---------------------------------------------------------------------------
// Inline spans
//
// Two constructs, and the choice of which two is the whole judgement here.
// `code` and **bold** are the ones a terminal can render without inventing
// anything: one is a colour, the other is a weight, and both exist at every
// fidelity down to a monochrome console.
//
// *Italic* is deliberately absent. `Modifier::ITALIC` is not written by
// `render::codes`, several terminals render it as reverse video, and a `*` in
// running prose is more often a footnote or a glob than an emphasis. Leaving the
// asterisks alone shows the reader exactly what the model wrote.
// ---------------------------------------------------------------------------

/// Split text into styled spans, resolving code spans and bold runs.
///
/// `base` is the style the surrounding block wanted; emphasis is applied *on
/// top* of it, so a bold word in a heading stays the heading's colour and a code
/// span in a quote is still dim behind its own colour.
fn inline(text: &str, skin: &Skin, base: Style) -> Vec<Span<'static>> {
    let chars: Vec<char> = text.chars().collect();
    let mut out: Vec<Span<'static>> = Vec::new();
    let mut plain = String::new();
    let mut i = 0;
    while i < chars.len() {
        // A code span: one backtick to the next one on the same line.
        if chars[i] == '`' {
            if let Some(end) = (i + 1..chars.len()).find(|j| chars[*j] == '`') {
                if end > i + 1 {
                    flush(&mut plain, &mut out, base);
                    out.push(Span::styled(
                        chars[i + 1..end].iter().collect::<String>(),
                        code_style(skin, base),
                    ));
                    i = end + 1;
                    continue;
                }
            }
        }
        // A bold run: `**` to the next `**`.
        if chars[i] == '*' && chars.get(i + 1) == Some(&'*') {
            if let Some(end) = (i + 2..chars.len().saturating_sub(1))
                .find(|j| chars[*j] == '*' && chars[j + 1] == '*')
            {
                if end > i + 2 {
                    flush(&mut plain, &mut out, base);
                    out.push(Span::styled(
                        chars[i + 2..end].iter().collect::<String>(),
                        base.add_modifier(Modifier::BOLD),
                    ));
                    i = end + 2;
                    continue;
                }
            }
        }
        plain.push(chars[i]);
        i += 1;
    }
    flush(&mut plain, &mut out, base);
    out
}

fn flush(plain: &mut String, out: &mut Vec<Span<'static>>, style: Style) {
    if !plain.is_empty() {
        out.push(Span::styled(std::mem::take(plain), style));
    }
}

/// Code, inline or fenced: [`Role::Info`], which is what a tool name and a path
/// already use. It is the palette's noun colour and code is a noun.
fn code_style(skin: &Skin, base: Style) -> Style {
    let mut style = skin.palette.style(Role::Info);
    // Whatever the block wanted kept — the dim of a quote, the bold of a
    // heading — with the colour replaced.
    style.add_modifier = base.add_modifier;
    style
}

// endregion: Inline spans

// region: Fitting to the window
// ---------------------------------------------------------------------------
// Fitting to the window
//
// Two ways to break a line and the difference between them is the point.
// `flow` breaks prose between words. `code_rows` breaks code at the exact
// column the window ends and nowhere else, so joining the rows reproduces the
// source. Both guarantee every row is at most `width` columns, which is what
// `frame.rs` reserves rows against.
// ---------------------------------------------------------------------------

/// Display columns, as ratatui will count them when it draws.
fn cols(text: &str) -> usize {
    Span::raw(text).width()
}

/// Wrap styled spans to the width, at word boundaries, with a hanging indent.
///
/// `lead` is drawn once on the first row — a bullet, a `>` — and counts towards
/// that row's width. `hang` is how far the rows after it are indented, so a
/// wrapped list item lines up under its own text.
fn flow(
    lead: Vec<Span<'static>>,
    body: Vec<Span<'static>>,
    width: usize,
    hang: usize,
) -> Vec<Line<'static>> {
    let hang = hang.min(width.saturating_sub(1));
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut used: usize = lead.iter().map(|s| cols(&s.content)).sum();
    let mut cur: Vec<Span<'static>> = lead;
    let mut has_word = false;
    let mut pending: Option<(String, Style)> = None;

    let wrap = |cur: &mut Vec<Span<'static>>,
                used: &mut usize,
                has_word: &mut bool,
                lines: &mut Vec<Line<'static>>| {
        lines.push(Line::from(std::mem::take(cur)));
        if hang > 0 {
            cur.push(Span::raw(" ".repeat(hang)));
        }
        *used = hang;
        *has_word = false;
    };

    for (word, space, style) in tokens(&body) {
        if space {
            if has_word {
                pending = Some((word, style));
            }
            continue;
        }
        let gap = pending.as_ref().map_or(0, |(t, _)| cols(t));
        if has_word && used + gap + cols(&word) > width {
            pending = None;
            wrap(&mut cur, &mut used, &mut has_word, &mut lines);
        }
        if let Some((text, style)) = pending.take() {
            used += cols(&text);
            cur.push(Span::styled(text, style));
        }
        // A word longer than the window has nowhere to go but across rows, and
        // a character boundary is the only honest place to put the break.
        let mut rest = word;
        loop {
            let room = width.saturating_sub(used);
            if cols(&rest) <= room {
                used += cols(&rest);
                cur.push(Span::styled(rest, style));
                has_word = true;
                break;
            }
            let (head, tail) = split_at_cols(&rest, room);
            if head.is_empty() {
                // No room at all on this row: take the wrap and try again.
                wrap(&mut cur, &mut used, &mut has_word, &mut lines);
                continue;
            }
            used += cols(&head);
            cur.push(Span::styled(head, style));
            has_word = true;
            rest = tail;
            wrap(&mut cur, &mut used, &mut has_word, &mut lines);
        }
    }
    lines.push(Line::from(cur));
    lines
}

/// Words and the whitespace between them, carrying each span's style.
fn tokens(spans: &[Span<'static>]) -> Vec<(String, bool, Style)> {
    let mut out = Vec::new();
    for span in spans {
        let mut run = String::new();
        let mut run_space = false;
        for ch in span.content.chars() {
            let space = ch.is_whitespace();
            if !run.is_empty() && space != run_space {
                out.push((std::mem::take(&mut run), run_space, span.style));
            }
            run_space = space;
            run.push(ch);
        }
        if !run.is_empty() {
            out.push((run, run_space, span.style));
        }
    }
    out
}

/// The longest prefix that fits in `room` columns, and what is left.
fn split_at_cols(text: &str, room: usize) -> (String, String) {
    let mut head = String::new();
    let mut used = 0;
    let mut chars = text.chars().peekable();
    while let Some(&ch) = chars.peek() {
        let w = cols(&ch.to_string());
        if used + w > room {
            break;
        }
        used += w;
        head.push(ch);
        chars.next();
    }
    (head, chars.collect())
}

/// A line inside a fenced block, split only where the window ends.
///
/// **Never at a space.** Wrapping code at a word boundary changes what it says;
/// the terminal would have broken it at the last column anyway, and breaking it
/// here as well means the row count `frame.rs` reserves is right.
fn code_rows(text: &str, width: usize, skin: &Skin) -> Vec<Line<'static>> {
    let style = skin.palette.style(Role::Info);
    if text.is_empty() {
        return vec![Line::from(Span::styled(String::new(), style))];
    }
    let mut rows = Vec::new();
    let mut rest = text.to_string();
    while !rest.is_empty() {
        let (head, tail) = split_at_cols(&rest, width);
        if head.is_empty() {
            // A single glyph wider than the whole window. Emit it rather than
            // spinning: a lost character is worse than a row that is one column
            // over on a terminal two columns wide.
            rows.push(Line::from(Span::styled(rest, style)));
            break;
        }
        rows.push(Line::from(Span::styled(head, style)));
        rest = tail;
    }
    rows
}

/// The line a fence becomes: a dim rule with the language written into it.
///
/// The fence itself is not printed — ` ``` ` on a screen is three characters of
/// syntax — and the rule takes its place, which is what makes the block look
/// like a block. It is the block's own row either way, so nothing a reader
/// copies out of the code gains a character.
fn rule(skin: &Skin, width: usize, info: &str) -> Line<'static> {
    let mark = skin.glyphs.rule;
    let lang: String = info.split_whitespace().next().unwrap_or("").to_string();
    let text = if lang.is_empty() {
        mark.repeat(width.min(72))
    } else {
        let head = mark.repeat(2);
        let tail_room = width.min(72).saturating_sub(cols(&head) + cols(&lang) + 2);
        format!("{head} {lang} {}", mark.repeat(tail_room))
    };
    Line::from(Span::styled(text, skin.palette.dim()))
}

/// What may reach a cell.
///
/// A model can put anything in its prose, including the escape byte that would
/// otherwise be written straight into the terminal — where it is a control
/// sequence rather than text, and where ratatui counts it as a column the
/// terminal does not. That is the same class of defect the status line already
/// refuses (`view.rs`), applied to the one other thing on screen that comes from
/// outside. Tabs become spaces because a tab in a cell has no width anybody
/// agrees on; everything else that is not printable is dropped.
///
/// `pub(crate)` since 2026-08-23, when the same defect turned up in
/// `render.rs`: tool stdout became spans without passing through here, so a
/// `Bash` call that emitted colour put those bytes in a redirected file. One
/// sanitiser, both doors.
pub(crate) fn sanitise(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        match ch {
            '\t' => out.push_str("    "),
            '\r' | '\n' => {}
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

// endregion: Fitting to the window

#[cfg(test)]
mod tests {
    use super::*;
    use crate::term::palette::{Level, Palette};
    use crate::term::render::{for_stream, plain, ASCII, UNICODE};

    fn skin(level: Level) -> Skin {
        Skin::new(Palette::new(level), UNICODE)
    }

    /// Render a whole document, as the streaming path would: one line at a
    /// time, in order, through one renderer.
    fn render(doc: &str, width: u16, skin: &Skin) -> Vec<Line<'static>> {
        let mut md = Markdown::new();
        doc.lines().flat_map(|l| md.line(l, width, skin)).collect()
    }

    fn text_of(lines: &[Line<'static>]) -> String {
        lines.iter().map(plain).collect::<Vec<_>>().join("\n")
    }

    // -----------------------------------------------------------------------
    // What the eye is supposed to see
    // -----------------------------------------------------------------------

    #[test]
    fn a_heading_is_bold_and_loses_its_hashes() {
        let s = skin(Level::Truecolor);
        let out = render("## The plan\n", 60, &s);
        assert_eq!(text_of(&out), "The plan");
        assert!(out[0].spans[0].style.add_modifier.contains(Modifier::BOLD));
        // Three levels, three weights of the same idea — and none of them is
        // the colour of ordinary prose, which is what makes them findable when
        // scrolling back.
        let h1 = render("# a\n", 60, &s)[0].spans[0].style.fg;
        let h2 = render("## a\n", 60, &s)[0].spans[0].style.fg;
        assert_ne!(h1, h2);
        // `#hashtag` and `#3` are prose. A heading needs its space.
        assert_eq!(text_of(&render("#3 failed\n", 60, &s)), "#3 failed");
    }

    #[test]
    fn bold_and_inline_code_are_styled_and_their_markers_go() {
        let s = skin(Level::Truecolor);
        let out = render("run **now** with `cargo test`\n", 60, &s);
        assert_eq!(text_of(&out), "run now with cargo test");
        let bold = out[0]
            .spans
            .iter()
            .find(|sp| sp.content.contains("now"))
            .unwrap();
        assert!(bold.style.add_modifier.contains(Modifier::BOLD));
        // The code span is coloured across every word of it — wrapping splits
        // spans, and a style that survived only the first word would look
        // right at this width and wrong at a narrower one.
        for word in ["cargo", "test"] {
            let code = out[0]
                .spans
                .iter()
                .find(|sp| sp.content.contains(word))
                .unwrap_or_else(|| panic!("{word} is missing: {out:?}"));
            assert_eq!(code.style.fg, Some(s.palette.color(Role::Info)));
        }
    }

    #[test]
    fn a_list_keeps_its_markers_in_one_column_and_hangs_the_wrap_under_the_text() {
        let s = skin(Level::Truecolor);
        let out = render("- alpha beta gamma delta epsilon\n- two\n", 20, &s);
        let rows = out.iter().map(plain).collect::<Vec<_>>();
        assert!(rows[0].starts_with("• alpha"), "{rows:?}");
        // The continuation is indented to where the text began, not to column
        // zero: a bullet that wraps must still read as one item.
        assert!(rows[1].starts_with("  "), "{rows:?}");
        assert!(!rows[1].trim_start().is_empty());
        // Every marker in the same column.
        let second = rows.iter().position(|r| r.contains("two")).unwrap();
        assert!(rows[second].starts_with("• two"), "{rows:?}");
        // Numbered lists keep their numbers, because the number is the content.
        let ordered = render("1. first\n2. second\n", 40, &s);
        assert!(text_of(&ordered).contains("1. first"));
    }

    #[test]
    fn prose_is_wrapped_at_words_to_the_width_it_was_given() {
        let s = skin(Level::None);
        let out = render("the quick brown fox jumps over the lazy dog\n", 20, &s);
        for line in &out {
            assert!(line.width() <= 20, "{:?} is too wide", plain(line));
        }
        // Words are intact: nothing was cut in the middle.
        let joined = out.iter().map(plain).collect::<Vec<_>>().join(" ");
        assert_eq!(
            joined.split_whitespace().collect::<Vec<_>>().join(" "),
            "the quick brown fox jumps over the lazy dog"
        );
    }

    // -----------------------------------------------------------------------
    // The three guarantees
    //
    // Each of these fails if the behaviour it names is removed, which is the
    // only kind of test worth having here.
    // -----------------------------------------------------------------------

    /// **A fenced code block is never reflowed.**
    ///
    /// Written as "the rows concatenate back to the source" rather than as a
    /// width check, because that is the property: a word-wrapped code line
    /// would still be under the width and would still be wrong.
    #[test]
    fn a_fenced_code_block_is_never_reflowed_and_never_loses_a_character() {
        let s = skin(Level::Truecolor);
        let code = "let total = compute(alpha, beta, gamma) + delta * epsilon;";
        let doc = format!("```rust\n{code}\n```\n");
        let out = render(&doc, 24, &s);
        // First and last rows are the fence rules; everything between is code.
        let body: String = out[1..out.len() - 1]
            .iter()
            .map(plain)
            .collect::<Vec<_>>()
            .concat();
        assert_eq!(body, code, "the code was reflowed: {out:?}");
        for line in &out {
            assert!(line.width() <= 24, "{:?}", plain(line));
        }
        // …and the same text outside a fence *is* broken at spaces, which is
        // what makes the assertion above a distinction rather than a tautology.
        let prose = render(code, 24, &s);
        assert!(prose.len() > 1);
        assert!(
            plain(&prose[0]).ends_with(|c: char| !c.is_whitespace())
                && plain(&prose[0]).contains(' '),
            "{:?}",
            plain(&prose[0])
        );
        assert_ne!(plain(&prose[0]).chars().count(), 24);
    }

    /// Markdown this file does not implement still reaches the screen.
    #[test]
    fn what_is_not_understood_is_printed_rather_than_swallowed() {
        let s = skin(Level::Truecolor);
        let doc = "| a | b |\n|---|---|\n| 1 | 2 |\n\
                   ![img](x.png) and *italics* and [a link](https://example.com)\n\
                   <div>html</div>\n";
        let out = text_of(&render(doc, 120, &s));
        for needle in [
            "| a | b |",
            "| 1 | 2 |",
            "*italics*",
            "[a link](https://example.com)",
            "![img](x.png)",
            "<div>html</div>",
        ] {
            assert!(out.contains(needle), "{needle} was swallowed: {out}");
        }
    }

    /// The guarantee that keeps `emma … | tee` safe, restated for this file:
    /// with no colour, nothing here produces an escape byte.
    #[test]
    fn with_no_colour_not_one_escape_byte_is_emitted() {
        let s = skin(Level::None);
        let doc = "# Title\n\ntext with **bold** and `code`\n\n```rust\nfn main() {}\n```\n\n- a\n> q\n---\n";
        for line in render(doc, 40, &s) {
            let out = for_stream(&s, &line);
            assert!(
                !out.contains('\x1b'),
                "an escape sequence reached a redirected stream: {out:?}"
            );
            assert_eq!(out, plain(&line));
        }
    }

    // -----------------------------------------------------------------------
    // Degrading, and the model's own bytes
    // -----------------------------------------------------------------------

    #[test]
    fn an_ascii_console_gets_ascii_and_nothing_else() {
        let s = Skin::new(Palette::new(Level::Ansi16), ASCII);
        let doc = "- bullet\n\n```\ncode\n```\n\n---\n";
        let out = text_of(&render(doc, 40, &s));
        assert!(out.is_ascii(), "mojibake on a legacy console: {out:?}");
        assert!(out.contains("- bullet"), "{out}");
    }

    /// A 16-colour terminal is sent named colours, never a hex — the palette
    /// owns that rule and this proves this file goes through the palette.
    #[test]
    fn a_sixteen_colour_terminal_is_never_sent_a_hex() {
        let s = skin(Level::Ansi16);
        for line in render("# h\n`code`\n```\nx\n```\n", 40, &s) {
            for span in &line.spans {
                assert!(
                    !matches!(span.style.fg, Some(ratatui::style::Color::Rgb(..))),
                    "{:?} was sent 24-bit colour",
                    span.content
                );
            }
        }
    }

    /// **What a model writes cannot become a control sequence.**
    ///
    /// The prose is the one thing on this screen that comes from outside, and
    /// it used to be written to the terminal byte for byte. ratatui counts a
    /// stored escape as one column and the terminal counts it as none, which
    /// makes the frame wrong about every row below it.
    #[test]
    fn an_escape_byte_in_the_models_prose_never_reaches_a_cell() {
        let s = skin(Level::Truecolor);
        let out = render("hello \x1b[31mred\x1b[0m \x07 and\ttabs\n", 80, &s);
        for line in &out {
            let text = plain(line);
            assert!(
                !text.chars().any(char::is_control),
                "a control byte reached a cell: {text:?}"
            );
        }
        // …and the words survived, which is what makes it a translation rather
        // than a refusal to draw.
        assert!(text_of(&out).contains("red"), "{:?}", text_of(&out));
        assert!(text_of(&out).contains("tabs"));
    }

    // -----------------------------------------------------------------------
    // Streaming
    // -----------------------------------------------------------------------

    /// Every row fits, whatever the document and whatever the width. This is
    /// the arithmetic `frame.rs` reserves rows against.
    #[test]
    fn no_row_is_ever_wider_than_the_window() {
        let s = skin(Level::Truecolor);
        let doc = "# a very long heading that will not fit anywhere at all\n\
                   - a bullet with a great many words in it, several of them long\n\
                   > quoted text that also runs on well past the end of the window\n\
                   ```rust\nlet x = supercalifragilisticexpialidocious_identifier_name;\n```\n\
                   plain prose, `code`, **bold**, and an unbreakablewordthatislongerthantheline\n";
        for width in [8u16, 12, 20, 40, 80] {
            for line in render(doc, width, &s) {
                assert!(
                    line.width() <= usize::from(width),
                    "width {width}: {:?} took {} columns",
                    plain(&line),
                    line.width()
                );
            }
        }
    }

    /// One malformed answer must not style the next one.
    #[test]
    fn an_unclosed_fence_does_not_leak_into_the_next_answer() {
        let s = skin(Level::Truecolor);
        let mut md = Markdown::new();
        md.line("```rust", 40, &s);
        md.line("fn main() {}", 40, &s);
        assert!(md.in_code());
        md.reset();
        assert!(!md.in_code());
        // …and the next line is prose again, with its markers rendered.
        let out = md.line("# Heading", 40, &s);
        assert_eq!(text_of(&out), "Heading");
    }

    /// A fence closes only on its own character. A ` ``` ` block containing a
    /// `~~~` line is one block, not two.
    #[test]
    fn a_fence_closes_only_on_the_mark_that_opened_it() {
        let s = skin(Level::Truecolor);
        let mut md = Markdown::new();
        md.line("~~~", 40, &s);
        md.line("```", 40, &s);
        assert!(md.in_code(), "a foreign fence closed the block");
        md.line("~~~", 40, &s);
        assert!(!md.in_code());
    }

    /// Backticks in a sentence are a code span, not a fence.
    #[test]
    fn inline_backticks_never_open_a_code_block() {
        let s = skin(Level::Truecolor);
        let mut md = Markdown::new();
        md.line("use `a` and `b` together", 60, &s);
        assert!(!md.in_code());
    }

    /// The quote marker is `> `, space and all, and a wrapped quote stays under
    /// its own text rather than starting at column zero where it would read as
    /// the end of the quotation.
    #[test]
    fn a_quote_keeps_its_marker_and_hangs_under_it() {
        let s = skin(Level::Truecolor);
        let rows: Vec<String> = render("> the terminal owns the transcript\n", 20, &s)
            .iter()
            .map(plain)
            .collect();
        assert!(rows[0].starts_with("> the"), "{rows:?}");
        assert!(rows[1].starts_with("  "), "{rows:?}");
    }

    #[test]
    fn a_blank_line_stays_a_blank_line() {
        let s = skin(Level::Truecolor);
        let out = render("a\n\nb\n", 40, &s);
        assert_eq!(out.len(), 3);
        assert_eq!(plain(&out[1]), "");
    }
}
