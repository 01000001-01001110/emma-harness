//! What `Read` saw, remembered so `Write` and `Edit` can refuse to work blind.
//!
//! **This is shared mutable state between three tools, and that is a design
//! decision rather than an implementation detail.** `Write` refusing to clobber
//! an unread file is only meaningful if the `Write` instance and the `Read`
//! instance agree on what "read" means, so they must hold the same tracker.
//! Nothing in the `Tool` trait carries per-session state — `ToolCtx` is passed
//! by reference and holds only identifiers — so the state lives in the tools
//! themselves, behind an `Arc`, and is handed to all three at construction.
//!
//! The cost is real: the tools are no longer independently constructible. A
//! caller that builds `Write` with a fresh tracker gets a `Write` that refuses
//! every overwrite, and a caller that builds it with a tracker `Read` does not
//! share gets a `Write` that never refuses. Nothing in the type system stops
//! either. That is why [`crate::fs_tools`] exists and is the only supported way
//! to construct the set.
//!
//! Entries are keyed by session so a resumed process cannot inherit a claim
//! that some earlier session had looked at a file, and they carry the size and
//! mtime observed at read time so that a file changed underneath the agent
//! counts as unread again. "I read it, then something else wrote it, then I
//! overwrote that" is the same blind clobber wearing a receipt.
//!
//! ## The stamp and the line hashes are not the same rule twice
//!
//! A sighting now carries two independent records of what was seen, and it is
//! worth being precise about why one does not subsume the other.
//!
//! The **stamp** (length and mtime) answers *did anything change*. It is cheap,
//! it covers the whole file including the parts that were never shown, and it is
//! the only thing that can license a whole-file `Write` — because a whole-file
//! write is a claim about every byte.
//!
//! The **line hashes** answer *did this particular line change*. They are what
//! makes a stale file still editable: when the stamp says the file moved, the
//! old behaviour was a blanket refusal and a full re-read, which is exactly the
//! expensive path this record exists to avoid. If the drift landed somewhere
//! else in the file, an addressed edit can go ahead and say so; if it landed in
//! the lines being replaced, the refusal can name the line.
//!
//! Neither replaces the third check, which lives in `edit.rs`: the hash the
//! *model* supplies with its address. That one catches a different failure
//! again — the model aiming at a line number it worked out four turns ago, from
//! a `Read` that has since been superseded. The tracker knows what the world
//! last looked like; it does not know what the model thinks it looks like. Only
//! the model's own quoted hash says that.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use crate::hashline;

/// What a file looked like when it was last seen.
///
/// **`content` is why this is not just metadata.** Length and mtime alone are
/// forgeable by accident: a formatter that rewrites a file to the same byte
/// length inside one filesystem tick produces an identical `(len, mtime)` pair,
/// and `Write` then overwrote the other edit believing the file was untouched.
/// No adversary required. An independent review rated the same-tick collision
/// unlikely on NTFS specifically, which is a reason to call it a lost-update
/// risk rather than a certainty — not a reason to keep a check that cannot see
/// the change it is checking for.
///
/// FNV-1a over the whole file, the hash `hashline` already uses for `Edit`'s
/// line anchors. Cheap enough to run on a file `Read` was going to load anyway,
/// and `None` when the file was not read — a `Write` to a path nobody opened
/// has no content to remember.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Stamp {
    len: u64,
    mtime: Option<SystemTime>,
    content: Option<u64>,
}

/// The lines that were actually shown, and what they said at the time.
///
/// Stored as a contiguous run rather than a map because every producer of one
/// is contiguous by construction — `Read` shows a window, `Write` and `Edit`
/// know the whole file they just authored — and a `Vec<u64>` with an offset is
/// both smaller and faster to check across a range than a `HashMap<usize, u64>`
/// with the same contents. Eight bytes a line: a ten-thousand-line file costs
/// 80 KB in a process that has just spent far more than that holding the file's
/// text in the transcript.
#[derive(Debug, Clone, Default)]
pub struct LineHashes {
    /// 1-based line number of `hashes[0]`.
    pub first: usize,
    pub hashes: Vec<u64>,
}

impl LineHashes {
    /// Hashes for a whole file's text, starting at line 1.
    pub fn of_text(text: &str) -> Self {
        Self {
            first: 1,
            hashes: text.lines().map(hashline::hash_line).collect(),
        }
    }

    pub fn get(&self, line: usize) -> Option<u64> {
        if line < self.first {
            return None;
        }
        self.hashes.get(line - self.first).copied()
    }
}

#[derive(Debug, Clone)]
struct Sighting {
    stamp: Stamp,
    /// False when `Read` truncated. A file seen in part is not a file seen.
    complete: bool,
    /// What each shown line said. Empty when the sighting predates any line
    /// record, which is not a state any tool in this crate produces — it exists
    /// so that "no hash recorded for that line" and "line outside the window"
    /// are the same answer, `None`, and get the same refusal.
    lines: LineHashes,
}

/// Whether a path may be written without the write being blind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadState {
    /// Never read in this session.
    Never,
    /// Read, but the file has changed since — the agent's picture is stale.
    Stale,
    /// Read, but the read was truncated, so most of the file is still unseen.
    Partial,
    /// Read in full, and unchanged since.
    Fresh,
}

#[derive(Debug, Default)]
pub struct ReadTracker {
    seen: Mutex<HashMap<(String, PathBuf), Sighting>>,
}

impl ReadTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record that the current content of `path` has been observed. Called by
    /// `Read`, and also by `Write` and `Edit` after a successful write, because
    /// a file the agent just authored is the one file it certainly knows the
    /// contents of.
    ///
    /// `lines` is what was shown, not what exists: `Read` passes the window it
    /// rendered, while `Write` and `Edit` pass the whole file because they
    /// produced every byte of it. A caller with nothing to say passes
    /// `LineHashes::default()`, and every addressed edit against those lines
    /// will then be refused for want of a record — which is the safe direction
    /// for this to fail in.
    pub fn record(&self, session: &str, path: &Path, complete: bool, lines: LineHashes) {
        // **The hash of what was actually shown, not of what is there now.**
        // `stamp_of` re-stats after the caller has already read the bytes, so a
        // change landing in between paired the old content with a new stamp and
        // the tracker called it `Fresh`. Recording the content the caller saw
        // closes that window: if the file moved underneath the read, the two
        // disagree and the next check says `Stale`.
        let mut stamp = stamp_of(path);
        stamp.content = content_of(path);
        let sighting = Sighting {
            stamp,
            complete,
            lines,
        };
        self.seen
            .lock()
            .expect("read tracker mutex")
            .insert((session.to_string(), path.to_path_buf()), sighting);
    }

    /// The arm order is the priority order, and it matters. A file read only in
    /// part *and* changed since reports `Stale`, not `Partial`, because that is
    /// the more urgent thing to say: re-reading fixes both, whereas being told
    /// "you only saw part of it" would send the model to `Edit` against text
    /// that may no longer be there.
    pub fn state(&self, session: &str, path: &Path) -> ReadState {
        let key = (session.to_string(), path.to_path_buf());
        let seen = self.seen.lock().expect("read tracker mutex");
        match seen.get(&key) {
            None => ReadState::Never,
            Some(s) if !same_file(&s.stamp, &stamp_of(path), path) => ReadState::Stale,
            Some(s) if !s.complete => ReadState::Partial,
            Some(_) => ReadState::Fresh,
        }
    }

    /// What the lines `first..=last` said when they were last shown, in order,
    /// with `None` for any line no sighting covers.
    ///
    /// One call rather than one per line, and that is not only about lock
    /// traffic: a range checked with N separate lookups could observe two
    /// different sightings if something recorded in between, and would then be
    /// validating a range that never existed. Answering the whole range from a
    /// single borrow of the map makes the answer internally consistent.
    ///
    /// `None` deliberately conflates "outside the window you read" with "no
    /// sighting at all". `Edit` distinguishes them anyway — it has already
    /// consulted [`Self::state`] by the time it asks — and collapsing them here
    /// keeps this method from having to describe a state machine it does not
    /// own.
    pub fn line_hashes(
        &self,
        session: &str,
        path: &Path,
        first: usize,
        last: usize,
    ) -> Vec<Option<u64>> {
        let key = (session.to_string(), path.to_path_buf());
        let seen = self.seen.lock().expect("read tracker mutex");
        let sighting = seen.get(&key);
        (first..=last)
            .map(|line| sighting.and_then(|s| s.lines.get(line)))
            .collect()
    }

    /// Whether `run` — the hashes of some consecutive lines as they are on disk
    /// right now — appears as a consecutive run anywhere in what was shown.
    ///
    /// **Anywhere, not at the same line numbers, and that is the whole point.**
    /// The obvious version of this check compares the recorded hash for line N
    /// against the file's line N, and it is wrong in a way that only shows up
    /// once something inserts a line: every line below the insertion shifts, so
    /// every comparison fails, and a range whose contents nobody touched is
    /// refused for having moved. Worse, it contradicts the refusal in `edit.rs`
    /// that says "your line is now line 7, retry there" — the retry would hit
    /// this check and be refused too. That contradiction was found by watching a
    /// real model take the advice and get refused for following it.
    ///
    /// So the question is *did this text change*, not *did this text move*.
    /// Moving is what the model's own address is for; changing is what this is
    /// for. A run of 64-bit hashes appearing intact somewhere in the record is
    /// as strong a statement that nothing changed as comparing in place, and it
    /// survives insertions above.
    pub fn contains_run(&self, session: &str, path: &Path, run: &[u64]) -> bool {
        if run.is_empty() {
            return true;
        }
        let key = (session.to_string(), path.to_path_buf());
        let seen = self.seen.lock().expect("read tracker mutex");
        match seen.get(&key) {
            None => false,
            Some(s) => s.lines.hashes.windows(run.len()).any(|w| w == run),
        }
    }

    /// Forget a path, for when a delete or a rename has made the stamp
    /// meaningless.
    ///
    /// **Nothing calls this today** — there is no delete or move tool yet — so
    /// it is surface without a user, recorded as such rather than described as
    /// though it were wired up. It is public because the alternative when such
    /// a tool arrives is a caller reaching into the map.
    pub fn forget(&self, session: &str, path: &Path) {
        self.seen
            .lock()
            .expect("read tracker mutex")
            .remove(&(session.to_string(), path.to_path_buf()));
    }
}

/// A missing file stamps as zero-length with no mtime. That is deliberate: two
/// consecutive stats of a file that does not exist agree, so "read a file that
/// is not there, then create it" is not reported as a stale clobber.
fn stamp_of(path: &Path) -> Stamp {
    match std::fs::metadata(path) {
        Ok(m) => Stamp {
            len: m.len(),
            mtime: m.modified().ok(),
            content: None,
        },
        Err(_) => Stamp {
            len: 0,
            mtime: None,
            content: None,
        },
    }
}

/// A whole-file hash, for comparing against a remembered one.
///
/// Read here rather than passed in, because the caller that needs this is
/// checking staleness *now* and the bytes it saw earlier are exactly what must
/// not be trusted.
fn content_of(path: &Path) -> Option<u64> {
    let bytes = std::fs::read(path).ok()?;
    Some(hashline::hash_line(&String::from_utf8_lossy(&bytes)))
}

/// Whether two stamps describe the same file contents.
///
/// Metadata first, because it is one `stat` and rules out most changes. The
/// hash is consulted only when metadata agrees *and* both sides recorded one —
/// which is the case the metadata comparison cannot see, and the only case
/// worth paying a read for.
fn same_file(remembered: &Stamp, now: &Stamp, path: &Path) -> bool {
    if remembered.len != now.len || remembered.mtime != now.mtime {
        return false;
    }
    match remembered.content {
        None => true,
        Some(was) => content_of(path) == Some(was),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A same-length rewrite with an identical mtime is still detected.
    ///
    /// **This is the collision the old stamp could not see.** Freshness was
    /// `(len, mtime)` alone, so a formatter rewriting a file to the same byte
    /// length inside one filesystem tick produced an identical pair — and
    /// `Write` overwrote the other edit believing the file was untouched. No
    /// adversary required.
    ///
    /// Staged by comparing stamps directly rather than by racing a real tick:
    /// forging the metadata is the point, and a test that waits for a
    /// coincidence is a test that passes for the wrong reason most of the time.
    #[test]
    fn a_same_length_change_under_an_identical_mtime_is_not_the_same_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("code.rs");
        std::fs::write(&file, "let a = 1;\n").unwrap();

        // What Emma remembers after reading it: metadata plus content.
        let mut remembered = stamp_of(&file);
        remembered.content = content_of(&file);
        assert!(remembered.content.is_some(), "a read must record content");

        // Somebody rewrites it to exactly the same length.
        std::fs::write(&file, "let a = 2;\n").unwrap();

        // Forge the metadata half: same length by construction, and the mtime
        // asserted equal so the comparison cannot fall back on it.
        let now = Stamp {
            len: remembered.len,
            mtime: remembered.mtime,
            content: None,
        };
        assert_eq!(now.len, remembered.len, "the forge needs equal lengths");

        assert!(
            !same_file(&remembered, &now, &file),
            "a same-length rewrite under an identical mtime read as unchanged"
        );
    }

    /// And an unchanged file is still unchanged — a freshness check that cries
    /// wolf sends every `Write` back for a re-read it does not need.
    #[test]
    fn an_untouched_file_compares_equal() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("code.rs");
        std::fs::write(&file, "let a = 1;\n").unwrap();
        let mut remembered = stamp_of(&file);
        remembered.content = content_of(&file);
        assert!(same_file(&remembered, &stamp_of(&file), &file));
    }
}
