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
///
/// Notably absent: `target`, `node_modules`, and anything else `.gitignore`
/// would list. Reading a build artefact is a legitimate thing for a coding
/// agent to want, and a hidden ignore list is a search that lies about what it
/// searched. The visit ceiling below is what keeps a large tree affordable,
/// and it says so when it fires.
const SKIP_DIRS: &[&str] = &[".git"];

pub struct Walked {
    pub files: Vec<PathBuf>,
    /// The visit ceiling fired, so the list is incomplete.
    pub truncated: bool,
}

/// The one sentence both `Glob` and `Grep` say when the ceiling above fires.
///
/// Shared for the same reason the walk is: two spellings of one cut drift, and
/// the day they disagree the model learns the ceiling is 200,000 from one tool
/// and something else from the other. It names the cap, says what the totals
/// beside it are worth once it has fired, and gives the only remedy there is —
/// there is no `max_visited` argument, and saying "retry" would send the model
/// to spend a turn on a knob that does not exist.
pub fn ceiling_notice() -> String {
    format!(
        "the walk stopped after visiting {MAX_VISITED} entries, so anything past that point was \
         never looked at and the counts here are a floor rather than a total; no argument raises \
         this ceiling — narrow `path` to a subdirectory to search a smaller tree"
    )
}

/// Files under `root`, symlinks not followed.
///
/// Not following links keeps the walk inside the containment boundary that
/// `path::resolve` established for `root`: a link out of the tree would
/// otherwise enumerate — and let `Grep` read — files outside it.
pub fn files(root: &Path) -> Walked {
    files_within(root, MAX_VISITED)
}

/// The walk itself, with the ceiling as an argument.
///
/// Split out for one reason and it is a test: proving the ceiling fires — and
/// that `truncated` is set when it does — otherwise costs a two-hundred-thousand
/// entry tree, and a guarantee that expensive to check is one nobody checks.
/// Production has exactly one caller and it passes [`MAX_VISITED`].
fn files_within(root: &Path, ceiling: usize) -> Walked {
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

    // `visited` counts entries the walker yielded — directories, unreadable
    // entries and files alike — which is the quantity the ceiling is meant to
    // bound, because the traversal is what costs, not the matching. Counting
    // only returned files would let a tree of a million empty directories run
    // unbounded while the counter stayed at zero.
    for entry in walker {
        visited += 1;
        if visited > ceiling {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The ceiling has to *fire* and it has to *say so*. Both halves matter:
    /// a walk that silently stopped hands `Glob` a short list it will report as
    /// the whole tree, which is the same lie as a truncated read presented as a
    /// short file.
    #[test]
    fn the_visit_ceiling_stops_the_walk_and_admits_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        for n in 0..10 {
            std::fs::write(dir.path().join(format!("f{n}")), "x").expect("write");
        }

        let whole = files_within(dir.path(), MAX_VISITED);
        assert_eq!(whole.files.len(), 10);
        assert!(
            !whole.truncated,
            "a walk that finished must not claim a cut"
        );

        let cut = files_within(dir.path(), 3);
        assert!(cut.files.len() < 10, "the ceiling did not bound anything");
        assert!(cut.truncated, "the walk stopped early and did not say so");
    }

    /// The sentence a cut walk hands to the model. It must name the number,
    /// admit the counts beside it are a floor, and offer a remedy that exists —
    /// the failure this whole mechanism was built after was advice to raise a
    /// limit that no argument reaches.
    #[test]
    fn the_ceiling_notice_names_the_cap_and_no_knob_that_does_not_exist() {
        let notice = ceiling_notice();
        assert!(notice.contains(&MAX_VISITED.to_string()), "{notice}");
        assert!(notice.contains("no argument raises"), "{notice}");
        assert!(notice.contains("`path`"), "no remedy offered: {notice}");
        // The remedy has to be the one that works. There is no ceiling
        // argument, and inviting a retry with a bigger one spends a turn on a
        // knob that is not there.
        assert!(!notice.contains("max_visited"), "{notice}");
    }
}
