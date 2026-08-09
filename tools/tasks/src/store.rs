//! Reading and writing the file, given that somebody else has it open.
//!
//! **You cannot lock a file a person is editing in VS Code.** An advisory lock
//! the editor does not take part in protects nothing, and a mandatory one turns
//! their save into an error dialog. So the only honest strategy is to hold the
//! file for as short a time as possible and to notice when it moved.
//!
//! Every mutating call therefore does the whole cycle inside one `invoke`:
//! read, parse, change one line, re-read to confirm nothing arrived, replace.
//! Nothing is cached across calls, so the agent can never write from a picture
//! of the file it formed a minute ago.
//!
//! **The confirmation is a hash of the bytes, not the mtime.** mtime has
//! second granularity on some filesystems, which is an eternity next to this
//! window, and a stamp that can silently fail to change is a stale-read guard
//! that fails open — the exact direction a guard must not fail in.
//!
//! **The replace is a temp file and a rename**, so a reader who opens the file
//! mid-write sees the old version or the new one and never half of each. On a
//! collision the change is re-applied to the newly read document rather than
//! forced on top of it, which is what makes a human's edit to a *different*
//! task survive an agent's edit to this one.
//!
//! **What this still cannot catch, said plainly.** An editor holds the whole
//! file in memory. If a person opened `tasks.md` at 10:00, Emma writes at
//! 10:01, and the person hits Ctrl-S at 10:02, their editor writes its entire
//! buffer and Emma's line is gone — no error, no conflict, because from the
//! filesystem's point of view that is simply a newer complete file. Nothing
//! this layer can do prevents it; only the editor's own "file changed on disk"
//! prompt does, and that is the editor's to offer. The window is also not
//! zero: a write landing between the confirming read and the rename is lost.
//! It is microseconds wide and it is not closed, only made small and admitted.

use std::path::{Path, PathBuf};

use emma_tool_api::{ToolCtx, ToolError};
use emma_tools_fs::path;

use crate::doc::{Doc, RELATIVE_PATH};

/// Where the list lives, resolved through the same containment check every
/// other tool uses. The path is a constant, so this looks like ceremony — it is
/// not: `.emma` is often a symlink into a shared configuration directory, and a
/// second containment implementation that disagreed with `tools/fs` about that
/// case is exactly the bug the reuse is here to prevent.
pub fn tasks_path(ctx: &ToolCtx) -> Result<PathBuf, ToolError> {
    let root = path::root(ctx)?;
    path::resolve(&root, RELATIVE_PATH)
}

/// The bytes as they were when we read them.
///
/// `None` means the file did not exist, which is distinct from an empty file:
/// creating a file someone else created in the meantime is still a collision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stamp(Option<u64>);

/// A missing file is an empty list, not a failure. The project simply has no
/// tasks yet.
pub fn load(file: &Path) -> Result<(Doc, Stamp), ToolError> {
    match std::fs::read(file) {
        Ok(bytes) => {
            let stamp = Stamp(Some(hash(&bytes)));
            let text = String::from_utf8(bytes).map_err(|_| {
                ToolError::Failed(format!(
                    "{RELATIVE_PATH} is not valid UTF-8; it cannot be a task list"
                ))
            })?;
            Ok((Doc::parse(&text), stamp))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok((Doc::empty(), Stamp(None))),
        Err(e) => Err(ToolError::Failed(format!(
            "{RELATIVE_PATH} could not be read: {e}"
        ))),
    }
}

/// Read, apply, confirm, write — retrying against a freshly read document when
/// the file moved underneath.
///
/// `change` must be re-runnable, because it will be re-run on a collision. It
/// returns `Err` when the change is impossible against that document (an id
/// that is not there), and that error is returned as-is rather than retried.
pub fn edit<T>(
    file: &Path,
    mut change: impl FnMut(&mut Doc) -> Result<T, ToolError>,
) -> Result<T, ToolError> {
    // Three, not one: two agents or an agent and a formatter can collide once
    // by chance. Three collisions in a row is a file being rewritten in a loop,
    // and retrying forever would hang the turn rather than report anything.
    for _ in 0..3 {
        let (mut doc, stamp) = load(file)?;
        let value = change(&mut doc)?;
        doc.stamp_ids();
        match replace_if_unchanged(file, stamp, &doc.render()) {
            Ok(()) => return Ok(value),
            Err(Collision) => continue,
        }
    }
    Err(ToolError::Failed(format!(
        "{RELATIVE_PATH} kept changing while it was being updated; \
         something else is writing it. Try again."
    )))
}

/// The file changed between the read and the write.
pub struct Collision;

/// Write `text` only if the file still hashes to `stamp`.
///
/// Public because the guard is the interesting part and a test needs to be able
/// to hand it a stamp that has gone stale; the retry above never surfaces one.
pub fn replace_if_unchanged(file: &Path, stamp: Stamp, text: &str) -> Result<(), Collision> {
    let current = match std::fs::read(file) {
        Ok(bytes) => Stamp(Some(hash(&bytes))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Stamp(None),
        // Unreadable now but readable a moment ago: treat it as changed rather
        // than overwrite something we cannot see.
        Err(_) => return Err(Collision),
    };
    if current != stamp {
        return Err(Collision);
    }
    write_atomically(file, text).map_err(|_| Collision)
}

fn write_atomically(file: &Path, text: &str) -> std::io::Result<()> {
    let parent = file.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent)?;
    // Beside the target, not in the system temp dir: a rename across
    // filesystems is a copy, and a copy is not atomic.
    let tmp = file.with_extension("md.tmp");
    std::fs::write(&tmp, text)?;
    match std::fs::rename(&tmp, file) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

fn hash(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}
