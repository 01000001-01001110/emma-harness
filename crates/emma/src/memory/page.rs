//! One memory, on disk: the frontmatter, the title, the body, and the slug a
//! title turns into.
//!
//! **The frontmatter reader is written here rather than handed to
//! `serde_yaml`, and that is a change from the fork this came from.** The
//! fork's argument for the crate was that it costs nothing — `emma-harness`
//! already reads skill and agent frontmatter with it. That is true of the
//! workspace and false of *this* crate: `crates/emma/Cargo.toml` does not
//! depend on `serde_yaml`, and adding a dependency was not this change's to
//! make. What is left is a parser for four keys whose writer is fifty lines
//! away, which is a much smaller problem than parsing YAML: the two sides
//! agree on quoting by construction, and the shape is fixed by [`Front`]
//! rather than by a document model.
//!
//! The part the fork was right to worry about is `source:`, which holds URLs —
//! `url: https://x/a#b: yes` is three colons, a `#`, and a token YAML would
//! read as a boolean. Three rules answer it, and each one is a test:
//!
//! - **A value is the rest of its line.** No comment stripping. A `#` in a
//!   frontmatter value is far more likely to be a URL fragment than a comment,
//!   and a reader that eats from `#` onwards silently shortens somebody's
//!   citation.
//! - **The writer quotes anything that is not plainly a word.** [`render`]
//!   emits a double-quoted, backslash-escaped scalar unless the value is
//!   unambiguous bare text, so a round trip cannot lose a colon, a leading
//!   dash or a trailing space.
//! - **`pinned` is `true` or `false` and nothing else.** `yes` reads as a
//!   malformed page rather than as a boolean, which is the same answer
//!   `serde_yaml` gives the harness's frontmatter one crate over. One input
//!   shape with two answers inside one program is a defect this repository has
//!   already paid for.
//!
//! Everything here tolerates CRLF and a leading UTF-8 BOM, because a page is a
//! file a person edits in whatever editor they have. That is not a hypothetical
//! either: a frontmatter parser in this workspace once dropped 86 of 90 real
//! files by matching `---\n` byte-exactly against fixtures written on LF.

use anyhow::{anyhow, Result};
use std::fmt;

/// The closed set. An unknown value is a malformed page, never a seventh
/// category: a wiki that invents its own would be invisible to every caller
/// that lists by category.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Category {
    Preferences,
    Projects,
    Facts,
    Workflows,
    People,
    References,
}

impl Category {
    /// Display and index order. Not alphabetical: it is the order a reader
    /// meets them in, from the settled to the incidental, and `index.md` is
    /// read by a human as often as by a model.
    pub const ALL: [Category; 6] = [
        Category::Preferences,
        Category::Projects,
        Category::Facts,
        Category::Workflows,
        Category::People,
        Category::References,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Category::Preferences => "preferences",
            Category::Projects => "projects",
            Category::Facts => "facts",
            Category::Workflows => "workflows",
            Category::People => "people",
            Category::References => "references",
        }
    }

    /// Parse a category from text — a frontmatter value, a CLI flag, a tool
    /// argument. The error names the whole closed set, because the caller that
    /// got it wrong is usually a model that guessed.
    pub fn parse(s: &str) -> Result<Category> {
        let want = s.trim().to_ascii_lowercase();
        Category::ALL
            .into_iter()
            .find(|c| c.as_str() == want)
            .ok_or_else(|| {
                anyhow!(
                    "`{s}` is not a memory category; expected one of {}",
                    Category::ALL.map(Category::as_str).join(", ")
                )
            })
    }
}

impl fmt::Display for Category {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The four frontmatter fields, and only those four.
///
/// **An unknown key is a malformed page, and that is the opposite call from
/// `AgentFront` in the harness.** That struct reads files written for another
/// program and must not break on their extra keys. This one reads files this
/// wiki owns, written against `schema.md`; a key nobody reads is a memory
/// whoever wrote it believes is stored and Emma will never show. Better to
/// report the page and have somebody look.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Front {
    pub category: Category,
    pub pinned: bool,
    /// ISO `YYYY-MM-DD`, the date the page was born. A string, not a date
    /// type: nothing here does arithmetic on it, and a string cannot silently
    /// reformat a hand-edited file on the next write.
    pub created: String,
    /// Where the knowledge came from — a session id, a file, a person, a URL.
    /// Free text, and the reason the writer quotes.
    pub source: String,
}

/// The keys, in the order [`render`] writes them. Also the vocabulary the
/// unknown-key error quotes back, so the two can never disagree.
const KEYS: [&str; 4] = ["category", "pinned", "created", "source"];

/// A page, as read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page {
    pub slug: String,
    /// The body's first `# ` heading, falling back to the slug. **The title
    /// lives in the markdown, not in the frontmatter**, because the
    /// frontmatter is fixed at four keys and because a human editing a memory
    /// expects its heading to be its name. The index shows this.
    pub title: String,
    pub front: Front,
    /// Everything after the frontmatter, verbatim, heading included.
    /// `[[wiki-links]]` are text and survive untouched — nothing in this layer
    /// resolves, rewrites, or validates them.
    pub body: String,
    pub archived: bool,
}

/// Split a page file. `Err` is the whole vocabulary of "malformed": no
/// frontmatter, unclosed frontmatter, an unparseable line, a repeated or
/// unknown key, a missing key, a bad boolean, an unknown category. Every one
/// of them says which page and what is wrong with it, because the message is
/// what reaches `index.md`'s unreadable section and, from there, a person.
pub fn parse(slug: &str, text: &str, archived: bool) -> Result<Page> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let rest = open(text)
        .ok_or_else(|| anyhow!("no frontmatter: a page must open with `---` and the four keys"))?;

    let mut category: Option<Category> = None;
    let mut pinned: Option<bool> = None;
    let mut created: Option<String> = None;
    let mut source: Option<String> = None;
    let mut closed = false;
    let mut consumed = 0usize;

    for raw_line in rest.split_inclusive('\n') {
        consumed += raw_line.len();
        let line = raw_line.trim_end_matches(['\n', '\r']);
        if line.trim_end() == "---" {
            closed = true;
            break;
        }
        if line.trim().is_empty() {
            continue;
        }
        let (key, value) = line
            .split_once(':')
            .ok_or_else(|| anyhow!("`{line}` is not a frontmatter `key: value` line"))?;
        let key = key.trim();
        let value = scalar(value.trim());
        let seen = |taken: bool| -> Result<()> {
            if taken {
                Err(anyhow!("`{key}` appears twice in the frontmatter"))
            } else {
                Ok(())
            }
        };
        match key {
            "category" => {
                seen(category.is_some())?;
                category = Some(Category::parse(&value)?);
            }
            "pinned" => {
                seen(pinned.is_some())?;
                pinned = Some(match value.as_str() {
                    "true" => true,
                    "false" => false,
                    other => {
                        return Err(anyhow!(
                            "`pinned: {other}` is not a boolean; write `true` or `false`"
                        ))
                    }
                });
            }
            "created" => {
                seen(created.is_some())?;
                created = Some(value);
            }
            "source" => {
                seen(source.is_some())?;
                source = Some(value);
            }
            other => {
                return Err(anyhow!(
                    "`{other}` is not a memory frontmatter key; expected one of {}",
                    KEYS.join(", ")
                ))
            }
        }
    }

    if !closed {
        return Err(anyhow!("the frontmatter is never closed by a `---` line"));
    }
    let missing = |k: &str| anyhow!("the frontmatter has no `{k}:` line");
    let front = Front {
        category: category.ok_or_else(|| missing("category"))?,
        // The only optional key. A page with no `pinned:` is not pinned, which
        // is what a hand-written page that never mentions it means.
        pinned: pinned.unwrap_or(false),
        created: created.ok_or_else(|| missing("created"))?,
        source: source.ok_or_else(|| missing("source"))?,
    };
    let body = rest[consumed..]
        .trim_start_matches(['\r', '\n'])
        .to_string();
    Ok(Page {
        slug: slug.to_string(),
        title: title_of(&body).unwrap_or_else(|| slug.to_string()),
        front,
        body,
        archived,
    })
}

/// `---` on the first line, whatever the file's line endings are. Same
/// tolerance as `harness::claude::split_agent`, and for the same measured
/// reason: a byte-exact `---\n` misses every CRLF file.
fn open(text: &str) -> Option<&str> {
    let rest = text.strip_prefix("---")?;
    let rest = rest.strip_prefix('\r').unwrap_or(rest);
    rest.strip_prefix('\n')
}

/// One frontmatter value, unquoted if it was quoted.
///
/// A bare value is taken to the end of its line and nothing is stripped from
/// it — see the module doc on `#`. A double-quoted value is unescaped, which
/// is the only way a value can carry a newline or a leading space back out of
/// a file. Single quotes are read too, because a person writing YAML by hand
/// reaches for them and `''` is the escape they will use.
fn scalar(value: &str) -> String {
    if let Some(inner) = wrapped(value, '"') {
        let mut out = String::with_capacity(inner.len());
        let mut chars = inner.chars();
        while let Some(c) = chars.next() {
            if c != '\\' {
                out.push(c);
                continue;
            }
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('r') => out.push('\r'),
                Some('t') => out.push('\t'),
                // Anything else after a backslash is itself, `\\` and `\"`
                // included. A parser that rejected here would turn a Windows
                // path in a `source:` into an unreadable page.
                Some(other) => out.push(other),
                None => out.push('\\'),
            }
        }
        return out;
    }
    if let Some(inner) = wrapped(value, '\'') {
        return inner.replace("''", "'");
    }
    value.to_string()
}

fn wrapped(value: &str, quote: char) -> Option<&str> {
    let inner = value.strip_prefix(quote)?.strip_suffix(quote)?;
    // `strip_suffix` on a one-character string would hand back the same quote
    // twice; a lone `"` is bare text, not an empty quoted value.
    (value.len() >= 2).then_some(inner)
}

/// Frontmatter plus body, as bytes to write.
pub fn render(front: &Front, body: &str) -> Result<String> {
    let body = body.trim_end();
    Ok(format!(
        "---\ncategory: {}\npinned: {}\ncreated: {}\nsource: {}\n---\n\n{body}\n",
        front.category,
        front.pinned,
        emit(&front.created),
        emit(&front.source),
    ))
}

/// A scalar as it goes onto disk: bare when it is plainly a word, quoted and
/// escaped otherwise.
///
/// The bare set is deliberately narrow — alphanumerics and a handful of
/// punctuation that cannot start a YAML construct or end a value ambiguously.
/// Quoting something that did not need it costs two bytes; not quoting
/// something that did costs a page.
fn emit(value: &str) -> String {
    let bare = !value.is_empty()
        && value.starts_with(|c: char| c.is_ascii_alphanumeric())
        && !value.ends_with(' ')
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || " ._-+/".contains(c));
    if bare {
        return value.to_string();
    }
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

fn title_of(body: &str) -> Option<String> {
    body.lines()
        .find_map(|l| l.trim_end().strip_prefix("# "))
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
}

/// A body that is guaranteed to carry the title as its `# ` heading, so the
/// round trip through [`parse`] returns the title it was written with.
pub fn with_heading(title: &str, body: &str) -> String {
    let body = body.trim();
    match title_of(body) {
        Some(_) => body.to_string(),
        None if body.is_empty() => format!("# {title}\n"),
        None => format!("# {title}\n\n{body}\n"),
    }
}

/// A title, lowercased into a filename.
///
/// Everything that is not ASCII alphanumeric becomes a separator, runs of
/// separators collapse, and the result is cut at 60 characters on a separator
/// boundary. Non-ASCII is dropped rather than transliterated: a slug is a
/// filename and a link target, the title keeps the real words, and guessing at
/// romanisation is how you get two pages that disagree about the same person.
/// A title with nothing left is `memory`, which then collision-numbers like
/// anything else.
///
/// **This is also the path guard.** The output is `[a-z0-9-]` and nothing
/// else, so no title can produce a `..`, a separator, a drive letter or a
/// device name; `Wiki::path_of` joins it under `pages/` or `archive/` and can
/// only ever land there.
pub fn slugify(title: &str) -> String {
    let mut s = String::new();
    for ch in title.chars() {
        if ch.is_ascii_alphanumeric() {
            s.push(ch.to_ascii_lowercase());
        } else if !s.ends_with('-') {
            s.push('-');
        }
    }
    let s = s.trim_matches('-');
    // Byte slicing is safe because the loop above emits ASCII only.
    let cut = if s.len() <= 60 {
        s
    } else {
        let head = &s[..60];
        head.rsplit_once('-').map(|(a, _)| a).unwrap_or(head)
    };
    match cut.trim_matches('-') {
        "" => "memory".to_string(),
        t => t.to_string(),
    }
}
