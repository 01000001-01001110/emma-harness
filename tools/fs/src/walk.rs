//! The one directory walk both `Glob` and `Grep` use.
//!
//! Shared so that "what does Emma consider part of the tree" has a single
//! answer. Two walkers drift, and the day they disagree is the day `Grep` finds
//! a file `Glob` swears does not exist.

use std::path::{Path, PathBuf};

use walkdir::WalkDir;

/// A ceiling on entries *visited*, not entries returned. Without it a walk
/// rooted at a home directory is unbounded, and the caps on returned results do
/// not help because the cost is in the traversal.
pub const MAX_VISITED: usize = 200_000;

/// Never descended into. Not a general ignore mechanism — the object store is
/// large, opaque and never what anyone meant, and anything more clever belongs
/// in a config file rather than compiled in.
const SKIP_DIRS: &[&str] = &[".git"];

pub struct Walked {
    pub files: Vec<PathBuf>,
    /// The visit ceiling fired, so the list is incomplete.
    pub truncated: bool,
}

/// Files under `root`, symlinks not followed.
///
/// Not following links keeps the walk inside the containment boundary that
/// `path::resolve` established for `root`: a link out of the tree would
/// otherwise enumerate — and let `Grep` read — files outside it.
pub fn files(root: &Path) -> Walked {
    let mut files = Vec::new();
    let mut visited = 0usize;
    let mut truncated = false;

    let walker = WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            !(e.file_type().is_dir()
                && e.file_name()
                    .to_str()
                    .is_some_and(|n| SKIP_DIRS.contains(&n)))
        });

    for entry in walker {
        visited += 1;
        if visited > MAX_VISITED {
            truncated = true;
            break;
        }
        // An unreadable subdirectory is skipped rather than failing the call:
        // "I could not open one directory" is not "the search failed", and
        // turning it into an error would make a single permission bit hide a
        // whole tree of results.
        let Ok(entry) = entry else { continue };
        if entry.file_type().is_file() {
            files.push(entry.into_path());
        }
    }

    Walked { files, truncated }
}

/// Modification time, newest first, path as the tiebreak so the order is
/// stable across runs and a test can assert on it.
pub fn sort_newest_first(files: &mut [PathBuf]) {
    files.sort_by(|a, b| {
        let ta = std::fs::metadata(a).and_then(|m| m.modified()).ok();
        let tb = std::fs::metadata(b).and_then(|m| m.modified()).ok();
        tb.cmp(&ta).then_with(|| a.cmp(b))
    });
}
