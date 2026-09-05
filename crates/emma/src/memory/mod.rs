//! A per-repository wiki of **distilled durable knowledge**.
//!
//! Ported from a divergent fork reviewed on 2026-08-27
//! (the divergent-fork audit reviewed on 2026-08-27), with one deliberate
//! subtraction: the fork also wrote the **verbatim body of every successful
//! `WebFetch`** into `.emma/memory/raw/web/`. That is not ported. Owner
//! ruling, 2026-08-27: the wiki keeps *durable knowledge distilled from* a
//! page, never the page.
//!
//! **Why the subtraction is a security fix and not a preference.** Verbatim
//! remote text inside the working tree is untrusted input sitting where a
//! later read treats it as the project's own notes — a prompt-injection
//! surface that also lands in a diff. The fork guarded it with a secret strip
//! its own comment declares is not a boundary. A distilled page is written
//! deliberately by whoever learned the thing, which removes the surface rather
//! than filtering it.
//!
//! # What this module is
//!
//! The *mechanical* half of a wiki, and all of it. Nothing here calls a model.
//! The library moves files, parses four frontmatter keys and rewrites two
//! bookkeeping files; the judgement — what is worth remembering, which page
//! owns a topic, whether two claims contradict — belongs to whoever is reading
//! [`SCHEMA`], which ships inside the binary and is installed on first touch.
//!
//! **The root is a parameter, never a global.** [`Wiki::project`] opens the
//! wiki inside a repository, at `<repo>/.emma/memory/`, and every repository
//! Emma works in accumulates its own. [`Wiki::global`] opens
//! `<home>/.emma/memory/` for facts about the user that outlive any one
//! project — the caller supplies `home`, so a test never touches the real one.
//! They are the same code with a different path, which is the only reason two
//! scopes cost nothing to have.
//!
//! **The index is not a cache.** `index.md` is regenerated from `pages/` after
//! every mutation, so it cannot drift from the ground truth by construction
//! rather than by discipline — see [`Wiki::rebuild_index`] for the price of
//! that choice. `log.md` is append-only and grep-shaped.
//!
//! **A malformed page is reported, never dropped.** [`Wiki::list`] returns the
//! pages it could read *and* a [`Malformed`] for each one it could not, with
//! the reason; `index.md` gets a section for them too. A parse failure in this
//! layer is a rig failure, and a rig failure must never look like a memory the
//! user never had.
//!
//! **Nothing here deletes.** [`Wiki::archive`] moves a file into `archive/`
//! whole, and [`Wiki::create`] numbers a colliding slug against `pages/` *and*
//! `archive/`, so a new memory can never land on an old one — including one
//! that was archived years earlier and would otherwise be silently resurrected
//! under a stranger's title.
//!
//! # What is on disk, and who can read it
//!
//! **Everything this module writes is plaintext markdown inside the
//! repository** — `pages/*.md`, `archive/*.md`, `index.md`, `log.md`,
//! `schema.md`. It is not encrypted, it is readable by anything with the file
//! open, and it is committable: a page written today is in every clone
//! tomorrow, and `git rm` does not remove it from history. So a memory is
//! prose about what was learned, never a credential, a token or a key.
//! `SCHEMA` says this to the model in the same words.
//!
//! # The wiring this expects
//!
//! Nothing in this crate calls into here yet; the writing path is a later
//! change at shared call sites. What that change needs from this module:
//!
//! - a way in — [`Wiki::project`] takes the repository root, and opening is
//!   the same call as creating, so there is no state where the directory
//!   exists and the discipline does not;
//! - the four mutations a maintainer performs — [`Wiki::create`],
//!   [`Wiki::update`], [`Wiki::pin`]/[`Wiki::unpin`], [`Wiki::archive`] — each
//!   of which does its own bookkeeping, so no caller can forget it;
//! - the two reads — [`Wiki::read`] by slug and [`Wiki::list`] by
//!   [`Filter`] — with errors that name the slug, because the caller that
//!   guessed it is usually a model;
//! - [`Wiki::view`], the seam a memory screen renders and nothing else from
//!   this module.
//!
//! Two decisions belong to that later change and are deliberately not made
//! here: whether the store is opened at all on a fresh repository, and whether
//! `.emma/memory/` is gitignored. This module's answer to the second one is
//! that a distilled page is worth committing — the archive exists so history
//! keeps what stopped being true — but the caller owns the default.

pub(crate) mod clock;
mod index;
mod page;

#[cfg(test)]
mod tests;

pub use page::{slugify, Category, Front, Page};

use anyhow::{anyhow, Context, Result};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// The maintainer discipline, compiled in: the pattern ships built in rather
/// than being something each user assembles. Installed once, then it belongs
/// to the user — [`Wiki::open`] never overwrites it.
pub const SCHEMA: &str = include_str!("schema.md");

/// How many entries [`Wiki::view`] hands a caller. A view is a card, not a
/// catalog; the catalog is `index.md` and the full list is [`Wiki::list`].
const RECENT: usize = 10;

/// A page the store could not read, and why. Never fatal, never silent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Malformed {
    pub slug: String,
    pub path: PathBuf,
    pub problem: String,
}

/// Whether a listing reaches into `archive/`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Archived {
    /// The default everywhere: archived memories are history, not state.
    #[default]
    Excluded,
    Included,
    Only,
}

/// What to list. Built with the chaining methods; `Filter::default()` is
/// "every live page".
#[derive(Debug, Clone, Default)]
pub struct Filter {
    pub category: Option<Category>,
    pub pinned: Option<bool>,
    pub archived: Archived,
}

impl Filter {
    pub fn category(mut self, c: Category) -> Self {
        self.category = Some(c);
        self
    }
    pub fn pinned(mut self, p: bool) -> Self {
        self.pinned = Some(p);
        self
    }
    pub fn archived(mut self, a: Archived) -> Self {
        self.archived = a;
        self
    }

    fn keeps(&self, p: &Page) -> bool {
        self.category.is_none_or(|c| c == p.front.category)
            && self.pinned.is_none_or(|want| want == p.front.pinned)
    }
}

/// Pages read, plus pages that could not be. Both halves are the answer.
#[derive(Debug, Clone, Default)]
pub struct Listing {
    /// Sorted by slug, so two calls agree and a diff of two listings means
    /// something.
    pub pages: Vec<Page>,
    pub malformed: Vec<Malformed>,
}

/// Live pages per category, and the totals a summary needs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Counts {
    pub per_category: BTreeMap<Category, usize>,
    /// Live pages. Archived and malformed are counted separately and are not
    /// in it: a memory that could not be read is not a memory that exists.
    pub total: usize,
    pub pinned: usize,
    pub archived: usize,
    pub malformed: usize,
}

impl Counts {
    pub fn of(&self, c: Category) -> usize {
        self.per_category.get(&c).copied().unwrap_or(0)
    }
}

/// One line of a catalog: everything a list row needs, without the body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub slug: String,
    pub title: String,
    pub category: Category,
    pub pinned: bool,
    pub created: String,
}

impl From<&Page> for Entry {
    fn from(p: &Page) -> Self {
        Entry {
            slug: p.slug.clone(),
            title: p.title.clone(),
            category: p.front.category,
            pinned: p.front.pinned,
            created: p.front.created.clone(),
        }
    }
}

/// **The seam.** Whatever renders the wiki renders one of these and nothing
/// else from this module; it is defined here so a screen and the store can be
/// built by two people at once and meet at a struct instead of at a merge
/// conflict. It carries the counts, the pinned memories, the most recently
/// touched ones — and the malformed list, because an honest empty state
/// includes "four pages are unreadable".
#[derive(Debug, Clone)]
pub struct MemorySummary {
    pub root: PathBuf,
    pub counts: Counts,
    pub pinned: Vec<Entry>,
    /// Most recently written first, capped at [`RECENT`].
    pub recent: Vec<Entry>,
    pub malformed: Vec<Malformed>,
}

/// One wiki, rooted anywhere.
#[derive(Debug, Clone)]
pub struct Wiki {
    root: PathBuf,
}

impl Wiki {
    /// Open the wiki at `root`, creating the layout and installing [`SCHEMA`]
    /// if it is absent. Init and open are one call on purpose: two would mean
    /// a state where the directory exists and the discipline does not.
    pub fn open(root: impl AsRef<Path>) -> Result<Self> {
        let root = root.as_ref().to_path_buf();
        let w = Wiki { root };
        for dir in [w.pages_dir(), w.archive_dir()] {
            fs::create_dir_all(&dir)
                .with_context(|| format!("creating the memory wiki at {}", dir.display()))?;
        }
        // Three files that are the user's the moment they exist. A reopen
        // rewriting any of them would eat a hand-edited schema, a hand-written
        // log entry, or a note somebody left at the top of the index.
        if !w.schema_path().exists() {
            fs::write(w.schema_path(), SCHEMA)?;
        }
        if !w.log_path().exists() {
            fs::write(
                w.log_path(),
                "# Memory log\n\nAppend-only. Oldest first.\n\n",
            )?;
        }
        if !w.index_path().exists() {
            w.rebuild_index()?;
        }
        Ok(w)
    }

    /// The wiki belonging to a repository: `<repo>/.emma/memory/`.
    pub fn project(repo: impl AsRef<Path>) -> Result<Self> {
        Wiki::open(repo.as_ref().join(".emma").join("memory"))
    }

    /// The wiki belonging to the user: `<home>/.emma/memory/`. Same code, and
    /// the caller supplies `home` so a test never touches the real one.
    pub fn global(home: impl AsRef<Path>) -> Result<Self> {
        Wiki::open(home.as_ref().join(".emma").join("memory"))
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn pages_dir(&self) -> PathBuf {
        self.root.join("pages")
    }
    pub fn archive_dir(&self) -> PathBuf {
        self.root.join("archive")
    }
    pub fn index_path(&self) -> PathBuf {
        self.root.join("index.md")
    }
    pub fn log_path(&self) -> PathBuf {
        self.root.join("log.md")
    }
    pub fn schema_path(&self) -> PathBuf {
        self.root.join("schema.md")
    }

    fn path_of(&self, slug: &str, archived: bool) -> PathBuf {
        let dir = if archived {
            self.archive_dir()
        } else {
            self.pages_dir()
        };
        dir.join(format!("{slug}.md"))
    }

    /// Write a new memory. The slug comes from the title; a slug already taken
    /// — **in `pages/` or in `archive/`** — gets a number, so creating a memory
    /// can never overwrite one and can never collide with an archived entry
    /// that a later `read` would then find first.
    pub fn create(
        &self,
        title: &str,
        category: Category,
        source: &str,
        body: &str,
    ) -> Result<Page> {
        let slug = self.free_slug(&page::slugify(title));
        let front = Front {
            category,
            pinned: false,
            created: clock::today(),
            source: source.to_string(),
        };
        let text = page::render(&front, &page::with_heading(title, body))?;
        fs::write(self.path_of(&slug, false), text)
            .with_context(|| format!("writing memory `{slug}`"))?;
        self.bookkeep("create", &slug, &format!("{category} — {title}"))?;
        self.read(&slug)
    }

    /// Rewrite a live memory's body, and optionally where it now came from.
    ///
    /// **The operation the schema calls "ingest" is mostly this one, not
    /// `create`.** A wiki whose only write is a new page accumulates six pages
    /// about one topic, which is the failure it exists to prevent — every fact
    /// has one home, and the second time you learn something about it you
    /// amend that home.
    ///
    /// `created` does not move: it is the page's birthday, not the date of its
    /// last edit, and `log.md` already carries when it changed. The slug does
    /// not move either, because it is a link target and every `[[slug]]`
    /// pointing at this page would otherwise break silently. Rename a page by
    /// putting a different `# ` heading in the body you pass.
    pub fn update(&self, slug: &str, body: &str, source: Option<&str>) -> Result<Page> {
        let mut p = self.read(slug)?;
        if p.archived {
            return Err(anyhow!(
                "memory `{slug}` is archived; archived pages are history and are not edited"
            ));
        }
        if let Some(s) = source {
            p.front.source = s.to_string();
        }
        let body = page::with_heading(&p.title, body);
        fs::write(self.path_of(slug, false), page::render(&p.front, &body)?)
            .with_context(|| format!("rewriting memory `{slug}`"))?;
        let after = self.read(slug)?;
        self.bookkeep("update", slug, &after.title)?;
        Ok(after)
    }

    fn free_slug(&self, base: &str) -> String {
        let taken = |s: &str| self.path_of(s, false).exists() || self.path_of(s, true).exists();
        if !taken(base) {
            return base.to_string();
        }
        (2..)
            .map(|n| format!("{base}-{n}"))
            .find(|s| !taken(s))
            .expect("an unbounded range always yields a free slug")
    }

    /// Read one memory by slug, live or archived. The error names the slug,
    /// because the caller that asked is usually a model that guessed it.
    pub fn read(&self, slug: &str) -> Result<Page> {
        for archived in [false, true] {
            let path = self.path_of(slug, archived);
            if path.is_file() {
                let text = fs::read_to_string(&path)
                    .with_context(|| format!("reading memory `{slug}`"))?;
                return page::parse(slug, &text, archived)
                    .with_context(|| format!("memory `{slug}` is malformed"));
            }
        }
        Err(anyhow!("no memory `{slug}` in {}", self.root.display()))
    }

    /// Every page matching the filter, plus every page that could not be read.
    pub fn list(&self, filter: Filter) -> Result<Listing> {
        let mut out = Listing::default();
        let dirs: &[(PathBuf, bool)] = &match filter.archived {
            Archived::Excluded => vec![(self.pages_dir(), false)],
            Archived::Only => vec![(self.archive_dir(), true)],
            Archived::Included => vec![(self.pages_dir(), false), (self.archive_dir(), true)],
        };
        for (dir, archived) in dirs {
            for (slug, path) in slugs_in(dir)? {
                match fs::read_to_string(&path)
                    .map_err(anyhow::Error::from)
                    .and_then(|t| page::parse(&slug, &t, *archived))
                {
                    Ok(p) if filter.keeps(&p) => out.pages.push(p),
                    Ok(_) => {}
                    Err(e) => out.malformed.push(Malformed {
                        slug,
                        path,
                        problem: e.to_string(),
                    }),
                }
            }
        }
        out.pages.sort_by(|a, b| a.slug.cmp(&b.slug));
        out.malformed.sort_by(|a, b| a.slug.cmp(&b.slug));
        Ok(out)
    }

    pub fn pin(&self, slug: &str) -> Result<Page> {
        self.set_pin(slug, true)
    }

    pub fn unpin(&self, slug: &str) -> Result<Page> {
        self.set_pin(slug, false)
    }

    /// Rewrite one flag and nothing else: `created` is the page's birthday and
    /// does not move, and the body is rewritten byte for byte.
    fn set_pin(&self, slug: &str, pinned: bool) -> Result<Page> {
        let mut p = self.read(slug)?;
        p.front.pinned = pinned;
        fs::write(
            self.path_of(slug, p.archived),
            page::render(&p.front, &p.body)?,
        )?;
        let op = if pinned { "pin" } else { "unpin" };
        self.bookkeep(op, slug, &p.title)?;
        self.read(slug)
    }

    /// Move a memory to `archive/`, whole. **Nothing in this module deletes a
    /// page**: the file moves so that a repository's history keeps it, which is
    /// the whole reason the archive is a directory and not a flag.
    pub fn archive(&self, slug: &str) -> Result<()> {
        let from = self.path_of(slug, false);
        if !from.is_file() {
            return Err(anyhow!(
                "no live memory `{slug}` to archive in {}",
                self.pages_dir().display()
            ));
        }
        // Read the title before the move, so a malformed page can still be
        // archived and still gets a log line that says what it was.
        let title = self
            .read(slug)
            .map(|p| p.title)
            .unwrap_or_else(|_| slug.to_string());
        fs::rename(&from, self.path_of(slug, true))
            .with_context(|| format!("archiving memory `{slug}`"))?;
        self.bookkeep("archive", slug, &title)
    }

    /// Live counts per category, and the pinned, archived and malformed
    /// totals.
    pub fn counts(&self) -> Result<Counts> {
        let live = self.list(Filter::default())?;
        let archived = self.list(Filter::default().archived(Archived::Only))?;
        let mut c = Counts {
            total: live.pages.len(),
            archived: archived.pages.len(),
            malformed: live.malformed.len() + archived.malformed.len(),
            ..Counts::default()
        };
        for p in &live.pages {
            *c.per_category.entry(p.front.category).or_default() += 1;
            c.pinned += usize::from(p.front.pinned);
        }
        Ok(c)
    }

    /// Rewrite `index.md` from `pages/` and `archive/` — the ground truth —
    /// and return what it wrote.
    ///
    /// **Every mutation calls this**, which makes the catalog correct by
    /// construction instead of by everyone remembering to patch it. The price
    /// is one full directory read per write. Index-first search tops out at
    /// hundreds of pages anyway, and re-reading hundreds of small files costs
    /// less than the model call that produced the memory; when a wiki outgrows
    /// that, this is the function to make incremental, and the test that pins
    /// a mutated index against a rebuilt one is what makes that safe to try.
    pub fn rebuild_index(&self) -> Result<String> {
        let all = self.list(Filter::default().archived(Archived::Included))?;
        let text = index::render(&all);
        fs::write(self.index_path(), &text)
            .with_context(|| format!("writing {}", self.index_path().display()))?;
        Ok(text)
    }

    /// Append one entry to `log.md`: `## [YYYY-MM-DD] <op> | <slug>` and one
    /// line under it — a shape `grep` can take apart.
    pub fn append_log(&self, op: &str, slug: &str, line: &str) -> Result<()> {
        use std::io::Write;
        let mut f = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.log_path())?;
        writeln!(f, "## [{}] {op} | {slug}\n{line}\n", clock::today())?;
        Ok(())
    }

    /// The invariant, in one private call: no mutation reaches the end of its
    /// function without rewriting the index and appending to the log.
    fn bookkeep(&self, op: &str, slug: &str, line: &str) -> Result<()> {
        self.rebuild_index()?;
        self.append_log(op, slug, line)
    }

    /// What a memory screen renders. See [`MemorySummary`] — this is the seam.
    pub fn view(&self) -> Result<MemorySummary> {
        let live = self.list(Filter::default())?;
        let archived = self.list(Filter::default().archived(Archived::Only))?;
        let counts = self.counts()?;

        let mut by_time: Vec<(SystemTime, &Page)> = live
            .pages
            .iter()
            .map(|p| (touched(&self.path_of(&p.slug, false)), p))
            .collect();
        // Newest first, slug breaking a tie. Ties are not rare — a filesystem
        // with coarse timestamps gives a whole burst of writes one mtime.
        //
        // The tie-break is redundant today and is kept anyway: `list` returns
        // pages already sorted by slug and `sort_by` is stable, so equal mtimes
        // hold slug order without it. A mutation run on 2026-08-27 confirmed
        // that — removing `then_with` left every test green. It stays because
        // the alternative is a total order that depends on a guarantee made in
        // another function, and the day `list` sorts by something else this
        // becomes the only thing keeping two calls on an untouched wiki from
        // disagreeing.
        by_time.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.slug.cmp(&b.1.slug)));

        let mut malformed = live.malformed;
        malformed.extend(archived.malformed);
        Ok(MemorySummary {
            root: self.root.clone(),
            counts,
            pinned: by_time
                .iter()
                .filter(|(_, p)| p.front.pinned)
                .map(|(_, p)| Entry::from(*p))
                .collect(),
            recent: by_time
                .iter()
                .take(RECENT)
                .map(|(_, p)| Entry::from(*p))
                .collect(),
            malformed,
        })
    }
}

/// `<slug>.md` files in a directory, sorted, ignoring everything else. A
/// missing directory is an empty one: a wiki whose `archive/` was removed by
/// hand should still list its pages.
fn slugs_in(dir: &Path) -> Result<Vec<(String, PathBuf)>> {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(anyhow::Error::from(e)).context(format!("reading {}", dir.display())),
    };
    let mut out = Vec::new();
    for entry in entries {
        let path = entry?.path();
        if path.extension().is_some_and(|e| e == "md") {
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                out.push((stem.to_string(), path.clone()));
            }
        }
    }
    out.sort();
    Ok(out)
}

fn touched(path: &Path) -> SystemTime {
    fs::metadata(path)
        .and_then(|m| m.modified())
        .unwrap_or(SystemTime::UNIX_EPOCH)
}
