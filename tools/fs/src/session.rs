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

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Stamp {
    len: u64,
    mtime: Option<SystemTime>,
}

#[derive(Debug, Clone, Copy)]
struct Sighting {
    stamp: Stamp,
    /// False when `Read` truncated. A file seen in part is not a file seen.
    complete: bool,
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
    /// `Read`, and also by `Write` after a successful write, because a file the
    /// agent just authored is the one file it certainly knows the contents of.
    pub fn record(&self, session: &str, path: &Path, complete: bool) {
        let sighting = Sighting {
            stamp: stamp_of(path),
            complete,
        };
        self.seen
            .lock()
            .expect("read tracker mutex")
            .insert((session.to_string(), path.to_path_buf()), sighting);
    }

    pub fn state(&self, session: &str, path: &Path) -> ReadState {
        let key = (session.to_string(), path.to_path_buf());
        let seen = self.seen.lock().expect("read tracker mutex");
        match seen.get(&key) {
            None => ReadState::Never,
            Some(s) if s.stamp != stamp_of(path) => ReadState::Stale,
            Some(s) if !s.complete => ReadState::Partial,
            Some(_) => ReadState::Fresh,
        }
    }

    /// Forget a path — used after a delete or rename would make the stamp
    /// meaningless. Kept public because the alternative is callers reaching
    /// into the map.
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
        },
        Err(_) => Stamp {
            len: 0,
            mtime: None,
        },
    }
}
