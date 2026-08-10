//! The format, and the parser that refuses to lose anything it did not write.
//!
//! `.emma/tasks/tasks.md` is not a scratchpad. It is a file a person has open
//! in an editor while the agent works, and every decision here follows from
//! that one fact.
//!
//! **Everything is a line, and a line nobody changed is written back byte for
//! byte.** The document model is `Vec<Line>`, not `Vec<Task>`. Headings, blank
//! lines, prose, a table, a task the writer would never have produced — all of
//! it is held as the exact bytes it arrived as, and `render` re-emits an
//! untouched line from `original` rather than from parsed fields. A model that
//! parsed into tasks and serialised from tasks would round-trip its own output
//! perfectly and quietly drop the sentence a human typed underneath a task,
//! which is the single failure that ends the feature: once someone's edit
//! disappears they stop reading the file, and a task list nobody reads is
//! theatre.
//!
//! The safety here is structural rather than careful. Nothing in the parser has
//! to *decide* to preserve a line it does not understand; a line it did not
//! recognise as a checkbox becomes a `Line::Raw` holding the original string,
//! and `Raw` has no other rendering. A tolerant parser eventually is not
//! tolerant; one that never re-renders what it did not touch always is. There
//! are exactly two exceptions and both are written down: a task line the caller
//! changed, and a duplicate handle (see `assign_ids`).
//!
//! **The shape of the file, in reading order.** [`Status`] is the three-way
//! glyph and its wire spelling; `TaskLine` is a parsed checkbox and `Line::Raw`
//! is everything else; [`Doc`] is the vector of those plus the operations the
//! tools need — `tasks`, `get`, `open_count`, `create`, `update`, `stamp_ids`.
//! Below `Doc` sit the free functions that do the actual recognising:
//! `parse_line` and `split_checkbox` decide what is a task, `split_id` pulls
//! the handle off the end, and `derive_id` / `first_free_id` / `assign_ids`
//! hand out the handles.
//!
//! **Status lives in the checkbox glyph, and nowhere else.** `[ ]` pending,
//! `[~]` in progress, `[x]` done. The obvious alternative — `## In progress` /
//! `## Done` sections — was rejected: making a heading authoritative means
//! changing a status *moves a line*, which relocates the note a human wrote
//! under it and reshuffles an order they chose. Rewriting one character in
//! place does neither. Headings a person adds are preserved and carry no
//! meaning, which is stated in the file's own preamble so nobody is surprised
//! by it. `[x]` is the one thing a human is most likely to type, and it means
//! exactly what they expect.
//!
//! **The id is visible, at the end of the line, in backticks.** An HTML comment
//! parses more cleanly and is invisible in a rendered preview — which is the
//! problem: what a person cannot see, they rewrite over without knowing it was
//! there. A visible `` `#a3f1` `` reads as a deliberate handle and survives
//! rewording, because rewording happens in the middle of a sentence and the
//! handle sits after the end of it.
//!
//! **An id is an annotation, not a requirement.** A checkbox line with no
//! handle is a real task: its id is derived from a hash of its text, so it is
//! addressable by `TaskGet` and `TaskUpdate` immediately, without a write, and
//! `TaskList` can stay genuinely read-only. The first time anything writes the
//! file, every unstamped task gains its handle — appended after the text,
//! never rewriting it — and from then on the id survives rewording too.
//!
//! **Nothing is ever reordered, and nothing completed is ever deleted.** See
//! `descriptions/task_update.md` for why cleanup is a tick rather than a
//! deletion.

// region: The file a person opens
// ---------------------------------------------------------------------------
// The file a person opens
//
// Where the list lives and what greets somebody who opens it for the first
// time. Both are addressed to the human rather than to the model, which is the
// reason this is markdown at all.
// ---------------------------------------------------------------------------

/// Fixed, because the path is the interface. A configurable location would
/// mean two agents in one project maintaining two lists neither knows about.
pub const RELATIVE_PATH: &str = ".emma/tasks/tasks.md";

/// The preamble a freshly created file gets. It is addressed to the human,
/// because the human is the reason the file is markdown at all — and it says
/// what is safe to edit, so that "I broke it" is never a reason not to touch
/// it.
const PREAMBLE: &str = "\
# Tasks

<!-- Emma maintains this file while it works. Edit it freely: reword a task,
     tick a box, add notes underneath one, reorder them, add headings of your
     own. Emma re-reads the file immediately before every write and keeps
     whatever it finds, including lines it did not write.

     `[ ]` not started   `[~]` in progress   `[x]` done
     Headings are yours; Emma reads the box, not the section.

     The `#abcd` at the end of a line is how Emma refers to that task. Reword
     the text as much as you like — keep the handle if you want the reference
     to survive. -->

";

// endregion: The file a person opens

// region: The document model
// ---------------------------------------------------------------------------
// The document model
//
// A vector of lines, not a vector of tasks. `TaskLine` is a checkbox the parser
// recognised and `Line::Raw` is everything else, held as the bytes it arrived
// as. `TaskView` is the flattened shape the tools hand to the model.
// ---------------------------------------------------------------------------

/// Three states, and the list is closed.
///
/// "Blocked" was asked for and refused. It is a relation, not a state: the
/// moment it exists as a variant it asks "blocked by what?", and this format
/// has nowhere to put the answer — there are no links between lines, only
/// lines. A status that cannot carry its own object degrades into a task
/// sitting at `[!]` forever with the reason living in somebody's head. A
/// blocked task is a pending task with a note underneath it saying what it is
/// waiting for, and notes are already preserved verbatim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Pending,
    InProgress,
    Completed,
}

impl Status {
    pub fn glyph(self) -> char {
        match self {
            Self::Pending => ' ',
            Self::InProgress => '~',
            Self::Completed => 'x',
        }
    }

    /// The spelling the model uses in arguments and reads in output. Kept
    /// distinct from the glyph so the file's cosmetics can change without
    /// changing the tool contract.
    pub fn wire(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::InProgress => "in_progress",
            Self::Completed => "completed",
        }
    }

    pub fn from_wire(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(Self::Pending),
            "in_progress" => Some(Self::InProgress),
            "completed" => Some(Self::Completed),
            _ => None,
        }
    }

    /// An unrecognised glyph is pending rather than an error. Someone typing
    /// `[?]` in an editor meant "a task", and refusing to see it would hide
    /// their work rather than surface it.
    fn from_glyph(c: char) -> Self {
        match c {
            'x' | 'X' => Self::Completed,
            '~' => Self::InProgress,
            _ => Self::Pending,
        }
    }

    pub fn is_open(self) -> bool {
        !matches!(self, Self::Completed)
    }
}

#[derive(Debug, Clone)]
struct TaskLine {
    indent: String,
    /// `-`, `*`, `+`, `1.` — whatever the human used, kept so a rewrite does
    /// not silently restyle their list.
    bullet: String,
    status: Status,
    text: String,
    id: String,
    /// The id was found in the file rather than derived from the text. An
    /// unstamped id is stable only while the text is; stamping fixes that.
    stamped: bool,
    /// `\r`, when the file uses CRLF. Preserved per line so a Windows-saved
    /// file does not silently become mixed-ending on the next write.
    eol: &'static str,
    original: String,
    dirty: bool,
}

impl TaskLine {
    /// Untouched lines take the early return and are the original bytes. The
    /// formatted branch is only ever reached for a line a call actually
    /// changed, and it is canonical rather than faithful: the human's indent
    /// and bullet are carried through, but the spacing *inside* the line is
    /// normalised to one space and the text is the trimmed text. That is a
    /// deliberate trade — the alternative is retaining offsets into a string
    /// whose middle is being replaced — and it is bounded to the one line the
    /// caller named. Anything that followed the handle cannot be lost here,
    /// because `split_id` only recognises a handle at the very end of the line.
    fn render(&self) -> String {
        if !self.dirty {
            return self.original.clone();
        }
        format!(
            "{}{} [{}] {} `#{}`{}",
            self.indent,
            self.bullet,
            self.status.glyph(),
            self.text,
            self.id,
            self.eol
        )
    }
}

#[derive(Debug, Clone)]
enum Line {
    Task(TaskLine),
    Raw(String),
}

/// One task as the tools see it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskView {
    pub id: String,
    pub status: Status,
    pub text: String,
    /// The lines a human wrote underneath, verbatim. Returned by `TaskGet` and
    /// deliberately left out of `TaskList`, which lands in the model's context
    /// on every turn.
    pub notes: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct Doc {
    lines: Vec<Line>,
}

// endregion: The document model

// region: Round-tripping
// ---------------------------------------------------------------------------
// Round-tripping
//
// Parse, change one thing, render. The property everything else rests on: what
// comes out equals what went in, apart from the lines a call actually named.
// ---------------------------------------------------------------------------

impl Doc {
    /// A file that does not exist is an empty document, not an error. The
    /// project has no tasks yet; that is a fact about the world.
    pub fn empty() -> Self {
        Self { lines: Vec::new() }
    }

    pub fn parse(text: &str) -> Self {
        // `split('\n')` rather than `lines()`: the trailing element carries
        // whether the file ended with a newline, so join reproduces the input
        // exactly instead of helpfully adding or removing one.
        let mut lines: Vec<Line> = text.split('\n').map(parse_line).collect();
        assign_ids(&mut lines);
        Self { lines }
    }

    /// The inverse of `parse`: rejoin on `\n` and let each line carry its own
    /// `\r`. A `Raw` line still has its carriage return inside its bytes and a
    /// `TaskLine` keeps it in `eol`, so a CRLF file stays CRLF and a mixed file
    /// stays mixed exactly as it arrived, line by line.
    pub fn render(&self) -> String {
        let mut out = String::new();
        for (i, line) in self.lines.iter().enumerate() {
            if i > 0 {
                out.push('\n');
            }
            match line {
                Line::Task(t) => out.push_str(&t.render()),
                Line::Raw(s) => out.push_str(s),
            }
        }
        out
    }

    pub fn tasks(&self) -> Vec<TaskView> {
        (0..self.lines.len()).filter_map(|i| self.view(i)).collect()
    }

    pub fn get(&self, id: &str) -> Option<TaskView> {
        self.index_of(id).and_then(|i| self.view(i))
    }

    /// The cheap question the loop wants to ask: is anything still open?
    /// Deliberately a count and not a bool, so a caller can say "3 left" rather
    /// than only "not done".
    pub fn open_count(&self) -> usize {
        self.lines
            .iter()
            .filter(|l| matches!(l, Line::Task(t) if t.status.is_open()))
            .count()
    }

    /// Append a task, returning its id.
    ///
    /// New tasks land immediately after the last existing task and its notes,
    /// not at the end of the file — a person who keeps a `## Notes` section at
    /// the bottom should not find tasks appearing underneath it.
    pub fn create(&mut self, text: &str, status: Status) -> String {
        let text = text.trim().to_string();
        let id = self.unique_id(&text);
        let line = TaskLine {
            indent: String::new(),
            bullet: "-".into(),
            status,
            text,
            id: id.clone(),
            stamped: true,
            eol: self.eol(),
            original: String::new(),
            dirty: true,
        };

        // A file with nothing in it but whitespace is treated as absent and
        // gains the preamble, because the preamble is the only place the human
        // is told the file is theirs to edit. The preamble is LF whatever
        // `eol()` decided above, so a file that was blank *and* CRLF comes back
        // with LF preamble lines and a CRLF task line. Stated rather than
        // hidden; there is no human text in a blank file to be lost by it.
        if self.lines.is_empty() || self.is_blank() {
            self.lines = PREAMBLE.split('\n').map(|s| Line::Raw(s.into())).collect();
        }
        let at = self.insertion_point();
        self.lines.insert(at, Line::Task(line));
        id
    }

    /// Returns false when no task carries that id — the caller turns that into
    /// `BadArguments`, because naming something that is not there is a fact
    /// about the call.
    pub fn update(&mut self, id: &str, status: Option<Status>, text: Option<&str>) -> bool {
        let Some(i) = self.index_of(id) else {
            return false;
        };
        let Line::Task(t) = &mut self.lines[i] else {
            return false;
        };
        if let Some(s) = status {
            t.status = s;
        }
        if let Some(new) = text {
            t.text = new.trim().to_string();
        }
        t.stamped = true;
        t.dirty = true;
        true
    }

    /// Give every task a durable handle. `store::edit` calls this once per
    /// attempt, immediately before rendering, so a save is the only thing that
    /// ever stamps — a read never writes, which is what keeps `TaskList`
    /// genuinely read-only.
    ///
    /// It is append-only: marking the line dirty makes `TaskLine::render`
    /// re-emit it with `` `#id` `` after the text, and the text itself is
    /// copied through. Along with the duplicate-handle rewrite in `assign_ids`
    /// this is one of the two ways a line the caller did not name can change,
    /// and both only ever add or replace a handle.
    pub fn stamp_ids(&mut self) {
        for line in &mut self.lines {
            if let Line::Task(t) = line {
                if !t.stamped {
                    t.stamped = true;
                    t.dirty = true;
                }
            }
        }
    }

    fn is_blank(&self) -> bool {
        self.lines.iter().all(|l| match l {
            Line::Raw(s) => s.trim().is_empty(),
            Line::Task(_) => false,
        })
    }

    fn insertion_point(&self) -> usize {
        if let Some(last) = self.lines.iter().rposition(|l| matches!(l, Line::Task(_))) {
            let mut at = last + 1;
            while at < self.lines.len() && self.is_note_of(last, at) {
                at += 1;
            }
            return at;
        }
        // No tasks yet: at the end, but *inside* the file's trailing newline
        // rather than after it. Appending past it leaves a file that does not
        // end in a newline, which every subsequent append then compounds.
        match self.lines.last() {
            Some(Line::Raw(s)) if s.is_empty() => self.lines.len() - 1,
            _ => self.lines.len(),
        }
    }

    /// Match the line ending the file already uses. A repository saved with
    /// CRLF should not acquire one LF line per agent run and a diff nobody
    /// asked for.
    fn eol(&self) -> &'static str {
        let crlf = self.lines.iter().any(|l| match l {
            Line::Task(t) => t.eol == "\r",
            Line::Raw(s) => s.ends_with('\r'),
        });
        if crlf {
            "\r"
        } else {
            ""
        }
    }

    /// A note is an indented, non-blank, non-task line following a task. Blank
    /// lines end the block: a paragraph separated by whitespace reads as
    /// belonging to the document, not to the task above it.
    fn is_note_of(&self, task: usize, at: usize) -> bool {
        let Line::Task(t) = &self.lines[task] else {
            return false;
        };
        match &self.lines[at] {
            Line::Task(_) => false,
            Line::Raw(s) => {
                let body = s.trim_end_matches('\r');
                !body.trim().is_empty() && indent_of(body).len() > t.indent.len()
            }
        }
    }

    fn view(&self, i: usize) -> Option<TaskView> {
        let Line::Task(t) = &self.lines[i] else {
            return None;
        };
        let mut notes = Vec::new();
        let mut at = i + 1;
        while at < self.lines.len() && self.is_note_of(i, at) {
            if let Line::Raw(s) = &self.lines[at] {
                notes.push(s.trim_end_matches('\r').trim().to_string());
            }
            at += 1;
        }
        Some(TaskView {
            id: t.id.clone(),
            status: t.status,
            text: t.text.clone(),
            notes,
        })
    }

    fn index_of(&self, id: &str) -> Option<usize> {
        let wanted = id.trim().trim_start_matches('#');
        self.lines
            .iter()
            .position(|l| matches!(l, Line::Task(t) if t.id.eq_ignore_ascii_case(wanted)))
    }

    fn unique_id(&self, text: &str) -> String {
        let taken: Vec<String> = self
            .lines
            .iter()
            .filter_map(|l| match l {
                Line::Task(t) => Some(t.id.clone()),
                Line::Raw(_) => None,
            })
            .collect();
        first_free_id(text, &taken)
    }
}

// endregion: Round-tripping

// region: Recognising a task
// ---------------------------------------------------------------------------
// Recognising a task
//
// Everything below is a reason to say no. A line promoted to a task acquires a
// handle on the next write, so over-recognising edits the human's prose; the
// cost of under-recognising is a task they have to restate.
// ---------------------------------------------------------------------------

fn parse_line(raw: &str) -> Line {
    let body = raw.strip_suffix('\r').unwrap_or(raw);
    let eol = if body.len() == raw.len() { "" } else { "\r" };
    match split_checkbox(body) {
        Some((indent, bullet, glyph, rest)) => {
            let (text, id) = split_id(rest);
            Line::Task(TaskLine {
                indent,
                bullet,
                status: Status::from_glyph(glyph),
                text,
                id: id.clone().unwrap_or_default(),
                stamped: id.is_some(),
                eol,
                original: raw.to_string(),
                dirty: false,
            })
        }
        None => Line::Raw(raw.to_string()),
    }
}

/// `  - [x] text` → (indent, bullet, glyph, text). Ordered and unordered
/// bullets both, because a hand-written list is as likely to be `1.` as `-`.
///
/// Everything here is a reason to say no. Recognising too much is the
/// expensive direction: a sentence promoted to a task acquires a handle on the
/// next write, and a handle appearing in the middle of somebody's paragraph is
/// the file editing them rather than the other way round. Returning `None`
/// only costs a task the human has to restate.
fn split_checkbox(line: &str) -> Option<(String, String, char, &str)> {
    let indent = indent_of(line).to_string();
    let rest = &line[indent.len()..];

    let bullet_len = if rest.starts_with("- ") || rest.starts_with("* ") || rest.starts_with("+ ") {
        1
    } else {
        let digits = rest.chars().take_while(|c| c.is_ascii_digit()).count();
        let after = rest.as_bytes().get(digits);
        if digits > 0 && matches!(after, Some(b'.') | Some(b')')) {
            digits + 1
        } else {
            return None;
        }
    };

    let bullet = rest[..bullet_len].to_string();
    let after = rest[bullet_len..].trim_start_matches(' ');
    if after.len() + bullet_len == rest.len() {
        return None; // no space after the bullet: not a list item
    }

    // Exactly one character between the brackets, checked on bytes. This looks
    // like it could split a multi-byte character below, and it cannot: a UTF-8
    // continuation byte is 0x80..=0xBF and a lead byte is >= 0xC2, so neither
    // can be `]` (0x5D). Byte 2 being `]` therefore proves byte 1 is a single
    // ASCII character, which is what makes `after[1..2]` safe.
    let bytes = after.as_bytes();
    if bytes.first() != Some(&b'[') || bytes.get(2) != Some(&b']') {
        return None;
    }
    let glyph = after[1..2].chars().next()?;
    let body = after[3..].trim_start_matches(' ');
    if body.len() == after.len() - 3 && !body.is_empty() {
        return None; // `[x]text` with no separating space is prose, not a task
    }
    Some((indent, bullet, glyph, body))
}

fn indent_of(line: &str) -> &str {
    let n = line
        .char_indices()
        .find(|(_, c)| !c.is_whitespace())
        .map(|(i, _)| i)
        .unwrap_or(line.len());
    &line[..n]
}

/// Split a trailing `` `#a3f1` `` handle off the text. Only at the very end,
/// so a `#hashtag` or an inline `` `#code` `` mid-sentence is left alone.
///
/// The shape has to be exact — backticks, a `#`, and one to eight hex digits —
/// because everything that is not a handle has to survive as part of the task's
/// own words. `derive_id` only ever emits four digits; the accepted range is
/// wider than the emitted one.
fn split_id(text: &str) -> (String, Option<String>) {
    let trimmed = text.trim_end();
    let Some(head) = trimmed.strip_suffix('`') else {
        return (trimmed.to_string(), None);
    };
    let Some(open) = head.rfind('`') else {
        return (trimmed.to_string(), None);
    };
    let candidate = &head[open + 1..];
    let Some(hex) = candidate.strip_prefix('#') else {
        return (trimmed.to_string(), None);
    };
    if hex.is_empty() || hex.len() > 8 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return (trimmed.to_string(), None);
    }
    (
        head[..open].trim_end().to_string(),
        Some(hex.to_ascii_lowercase()),
    )
}

// endregion: Recognising a task

// region: Ids
// ---------------------------------------------------------------------------
// Ids
//
// A handle is an annotation, not a requirement: an unstamped task's id is
// derived from its own words, which is what lets a read hand out an id without
// writing one. Stamping makes it survive rewording as well.
// ---------------------------------------------------------------------------

/// FNV-1a over the task's own words. Derived rather than random so that a task
/// with no handle still has a stable id across two calls that never wrote the
/// file — which is what lets `TaskList` stay read-only and still hand the model
/// something it can pass to `TaskGet`.
fn derive_id(text: &str, salt: u32) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in text.trim().as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    for b in salt.to_le_bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{:04x}", h & 0xffff)
}

/// The derived id, re-salted until it is one this document is not already
/// using. Salting rather than counting keeps the id opaque: a handle that read
/// as `#0001`, `#0002` would invite somebody to renumber them.
///
/// The bound is honest about what it is: 16 bits is 65,536 handles, so a single
/// file holding that many tasks would exhaust the space and reach the panic.
/// The panic is preferred to a silent duplicate, which is the failure `TaskGet`
/// cannot see.
fn first_free_id(text: &str, taken: &[String]) -> String {
    for salt in 0..u32::MAX {
        let id = derive_id(text, salt);
        if !taken.iter().any(|t| t == &id) {
            return id;
        }
    }
    unreachable!("16 bits of id space cannot be exhausted by one file")
}

/// Fill in ids after parsing, in document order.
///
/// A duplicate id — two stamped lines carrying the same handle, which only
/// happens when a human copies a line — is re-derived for the later one rather
/// than tolerated. `TaskGet` would otherwise silently answer about the first,
/// and quietly picking one of two things the caller might have meant is the
/// same mistake `Edit` refuses to make with an ambiguous anchor. This is the
/// one input for which parse → render is not the identity, deliberately.
fn assign_ids(lines: &mut [Line]) {
    let mut taken: Vec<String> = Vec::new();
    for line in lines.iter_mut() {
        let Line::Task(t) = line else { continue };
        if t.stamped && !taken.contains(&t.id) {
            taken.push(t.id.clone());
            continue;
        }
        if t.stamped {
            // Collided. It needs a new handle written down, so it is dirty.
            t.dirty = true;
        }
        let id = first_free_id(&t.text, &taken);
        taken.push(id.clone());
        t.id = id;
    }
}

// endregion: Ids

// region: Tests
// ---------------------------------------------------------------------------
// Tests
//
// The format's own guarantees, at the level they are implemented. Every one of
// these is a decision from the module doc made falsifiable: bytes back
// unchanged, the glyph outranking the heading, a handle outliving its words,
// prose staying prose, and CRLF staying CRLF.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_untouched_document_renders_byte_for_byte() {
        // The property everything else rests on. Note the deliberate oddities:
        // a `*` bullet, a hand-ticked box, no trailing newline.
        let src = "## Later\n\n* [x] shipped it\n  because Tuesday\n\nprose\n";
        assert_eq!(Doc::parse(src).render(), src);
        let no_newline = "- [ ] one";
        assert_eq!(Doc::parse(no_newline).render(), no_newline);
    }

    /// The fixture is adversarial on purpose: a pending task under a `## Done`
    /// heading. Any implementation that ever grew section awareness reads this
    /// as completed, which is the whole reason sections were rejected.
    #[test]
    fn the_glyph_is_the_status_and_the_heading_is_not() {
        let doc = Doc::parse("## Done\n\n- [ ] not actually done\n");
        assert_eq!(doc.tasks()[0].status, Status::Pending);
        assert_eq!(doc.open_count(), 1);
    }

    /// The handle is only worth being visible if it outlives the words around
    /// it. Every word changes here, so an id derived from the text at render
    /// time rather than carried would come out different and the model's
    /// reference would break on the call that changed the least.
    #[test]
    fn a_handle_survives_a_rewording_and_a_reworded_task_keeps_it() {
        let mut doc = Doc::parse("- [ ] the old words entirely `#beef`\n");
        assert!(doc.update("beef", None, Some("completely different words")));
        assert!(doc.render().contains("completely different words `#beef`"));
    }

    #[test]
    fn an_unstamped_task_is_addressable_without_a_write() {
        let doc = Doc::parse("- [ ] write the parser\n");
        let id = doc.tasks()[0].id.clone();
        assert!(doc.get(&id).is_some(), "derived id does not resolve");
        // Stable: the same text parsed again yields the same handle, which is
        // what makes a read-only TaskList able to hand out ids at all.
        assert_eq!(Doc::parse("- [ ] write the parser\n").tasks()[0].id, id);
    }

    #[test]
    fn duplicate_handles_are_separated_rather_than_shadowed() {
        let doc = Doc::parse("- [ ] a `#1111`\n- [ ] b `#1111`\n");
        let ids: Vec<String> = doc.tasks().into_iter().map(|t| t.id).collect();
        assert_ne!(ids[0], ids[1], "TaskGet would have been ambiguous");
    }

    /// Note attachment decides two things at once: what `TaskGet` returns, and
    /// where `create` inserts. The fixture separates the two tasks with a blank
    /// line and unindented prose, so a rule that ran to the next task instead
    /// of stopping at the blank line would hand task `b` somebody else's note
    /// and drop new tasks below unrelated paragraphs.
    #[test]
    fn notes_belong_to_the_task_above_them() {
        let doc = Doc::parse("- [ ] a\n  see src/x.rs\n\nunrelated prose\n- [ ] b\n");
        assert_eq!(doc.tasks()[0].notes, vec!["see src/x.rs".to_string()]);
        assert!(doc.tasks()[1].notes.is_empty());
    }

    /// Three near-misses, one per rejection in `split_checkbox`: a checkbox not
    /// at the start of the line, a bullet with no space after it, and brackets
    /// with nothing between them. Recognising any of these promotes a sentence
    /// to a task, and the next write stamps a handle into the middle of
    /// somebody's paragraph.
    #[test]
    fn prose_that_looks_like_a_checkbox_is_left_as_prose() {
        for line in ["not a task [ ] here", "-[ ] no space", "- [] too short"] {
            let doc = Doc::parse(line);
            assert!(doc.tasks().is_empty(), "{line} parsed as a task");
        }
    }

    /// A *modified* line is the only place the ending can be lost, because an
    /// untouched line carries its `\r` inside its original bytes and cannot get
    /// this wrong. Without this, every agent run on a Windows checkout would
    /// convert one line per update and produce a diff nobody asked for.
    #[test]
    fn crlf_stays_crlf() {
        let src = "- [ ] one `#0001`\r\n";
        let mut doc = Doc::parse(src);
        doc.update("0001", Some(Status::Completed), None);
        assert_eq!(doc.render(), "- [x] one `#0001`\r\n");
    }
}

// endregion: Tests
