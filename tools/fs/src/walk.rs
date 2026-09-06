//! The one directory walk both `Glob` and `Grep` use.
//!
//! Shared so that "what does Emma consider part of the tree" has a single
//! answer. Two walkers drift, and the day they disagree is the day `Grep` finds
//! a file `Glob` swears does not exist.

use std::path::{Path, PathBuf};

use ignore::gitignore::{Gitignore, GitignoreBuilder};

/// A ceiling on entries *visited*, not entries returned. Without it a walk
/// rooted at a home directory is unbounded, and the caps on returned results do
/// not help because the cost is in the traversal.
pub const MAX_VISITED: usize = 200_000;

/// Never descended into, whatever the ignore setting. The object store is
/// large, opaque and never what anyone meant, and `Read` reaches a loose object
/// by name for the one caller in a thousand who wants one.
const SKIP_DIRS: &[&str] = &[".git"];

/// How many ignored paths the notice names before it stops listing.
const NAMED_IGNORES: usize = 6;

/// Whether the walk honours the `.gitignore` files it finds.
///
/// # Why there is a choice here at all
///
/// This file used to skip nothing but `.git`, on the argument that reading a
/// build artefact is a legitimate thing for a coding agent to want and that a
/// hidden ignore list is a search that lies about what it searched. The
/// principle is right. The mechanism failed it.
///
/// Measured in Emma's own repository: `target/` holds 255,636 entries and the
/// rest of the tree holds 351. The visit ceiling below therefore fired inside
/// build output every time, and `Glob **/*.md` returned 27 of the 107 markdown
/// files in the repository while reporting "27 of 27 paths". The ceiling notice
/// was attached, but a notice saying "the counts here are a floor" alongside a
/// confident total is not what a reader takes from it, and a model searching
/// this repository silently missed 80 of 107 files.
///
/// Re-certified on Windows, 2026-09-06, by
/// [`tests::certify_gitignore_walk_against_this_repository`] against this
/// checkout: 351 files honouring the ignore files against 163,848 including
/// them, 9 ignored paths named `.env, blog, memory, notes, target,
/// verification`, and 38 markdown files against 324. The shape is the same one
/// measured on macOS with a smaller `target/`: the ignore skip is three orders
/// of magnitude, so a `Glob` result cap of 1,000 is spent entirely inside build
/// output before any source is reached, whether or not the visit ceiling
/// happens to fire on the day. It also answers the cost question the hand-rolled
/// traversal raised — 6 ms honouring, 460 ms walking everything, debug build.
///
/// So both halves have to hold, and neither is negotiable:
///
/// 1. The walk reaches the source. `.gitignore` is honoured by default, which
///    is what `ripgrep` does and what makes the ceiling stop firing.
/// 2. The search never hides what it skipped. A declared ignore list is not a
///    hidden one: every result that skipped anything says how many paths and
///    names the first few, and both tools take an `include_ignored` argument
///    that turns the skipping off. See [`ignored_notice`].
///
/// The rejected alternatives, since the ceiling was the third:
///
/// - *A much larger ceiling.* Raises the cost of every wrong-shaped call rather
///   than fixing the ordering problem, and at 255,000 entries of build output
///   the useful files are still last.
/// - *A per-directory entry budget.* Cheaper, but it truncates by an arbitrary
///   rule the model cannot predict or name, which is the failure being fixed.
/// - *Ordering source before ignored paths.* Fixes `Glob`'s newest-first list
///   and not `Grep`, which reads every candidate.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Ignores {
    /// `.gitignore` is honoured and what it removed is reported.
    Honour,
    /// Nothing is skipped but `.git`. The escape hatch behind `include_ignored`.
    Walk,
}

pub struct Walked {
    pub files: Vec<PathBuf>,
    /// The visit ceiling fired, so the list is incomplete.
    pub truncated: bool,
    /// Paths an ignore file removed, counted where the walk turned back: a
    /// skipped directory counts once, not once per file inside it. Counting the
    /// contents would mean walking them, which is the cost this avoids.
    pub ignored: usize,
    /// The first few of those, relative to the walk root, for the notice.
    pub ignored_names: Vec<String>,
    /// Directories the walk could not open, and therefore did not search.
    ///
    /// **A skip is correct; a silent skip is a wrong answer.** Refusing the
    /// whole call because one directory is unreadable would let a single
    /// permission bit hide a tree of results, so skipping is right and the
    /// comment below has always said so. What was missing is that nobody was
    /// told: `Grep` returned `no matches` for a file it never looked at, and a
    /// reviewer certified it live by denying read on a subdirectory and
    /// watching a matching file vanish from the results with no count and no
    /// cut notice.
    ///
    /// `DEF-002` fixed exactly this one layer down — an unreadable *file* is
    /// counted and named — and `tool-api`'s rule is that *"I could not look"*
    /// and *"I looked and there was nothing"* must never render as the same
    /// message. A directory is the same sentence about more files.
    ///
    /// Paths rather than a bare count, because "3 directories" sends the reader
    /// nowhere and `secret/` sends them straight to the permission bit.
    pub unreadable: Vec<PathBuf>,
}

/// The sentence `Glob` and `Grep` both say when the walk could not open
/// something, in the same voice as [`ceiling_notice`].
///
/// Named, capped and honest about the remedy: there is no argument that makes
/// an unreadable directory readable, and saying "retry" would send the model to
/// spend a turn on a knob that does not exist.
pub fn unreadable_notice(paths: &[PathBuf]) -> String {
    const SHOWN: usize = 5;
    let names: Vec<String> = paths
        .iter()
        .take(SHOWN)
        .map(|p| p.display().to_string())
        .collect();
    let more = paths.len().saturating_sub(names.len());
    let tail = if more > 0 {
        format!(", and {more} more")
    } else {
        String::new()
    };
    format!(
        "{} director{} could not be opened and {} NOT searched, so anything inside {} missing \
         from these results rather than absent: {}{tail}. No argument changes that — the \
         permission or the lock has to change",
        paths.len(),
        if paths.len() == 1 { "y" } else { "ies" },
        if paths.len() == 1 { "was" } else { "were" },
        if paths.len() == 1 { "it is" } else { "them is" },
        names.join(", "),
    )
}

/// The one sentence both `Glob` and `Grep` say when the ceiling above fires.
///
/// Shared for the same reason the walk is: two spellings of one cut drift, and
/// the day they disagree the model learns the ceiling is 200,000 from one tool
/// and something else from the other. It names the cap, says what the totals
/// beside it are worth once it has fired, and gives the only remedy there is.
/// There is no `max_visited` argument, and saying "retry" would send the model
/// to spend a turn on a knob that does not exist.
///
/// Since `.gitignore` is honoured by default this fires far less often, and
/// when it does it is now usually true: a tree with 200,000 tracked entries is
/// a tree worth narrowing.
pub fn ceiling_notice() -> String {
    format!(
        "the walk stopped after visiting {MAX_VISITED} entries, so anything past that point was \
         never looked at and the counts here are a floor rather than a total; no argument raises \
         this ceiling — narrow `path` to a subdirectory to search a smaller tree"
    )
}

/// The sentence every result that skipped an ignored path carries.
///
/// This is the whole price of honouring `.gitignore`. The skip is declared in
/// the result rather than compiled in silently, it names what it removed so the
/// model can see whether the thing it wanted is in there, and it names the
/// argument that turns it off. A result that skipped nothing says nothing.
pub fn ignored_notice(ignored: usize, names: &[String]) -> String {
    let named = if names.is_empty() {
        String::new()
    } else {
        let more = ignored.saturating_sub(names.len());
        let tail = if more > 0 {
            format!(" and {more} more")
        } else {
            String::new()
        };
        format!(" ({}{tail})", names.join(", "))
    };
    format!(
        "{ignored} paths ignored by `.gitignore` were skipped{named}, along with anything inside \
         them; pass include_ignored=true to search them"
    )
}

/// Files under `root`, symlinks not followed.
///
/// Not following links keeps the walk inside the containment boundary that
/// `path::resolve` established for `root`: a link out of the tree would
/// otherwise enumerate — and let `Grep` read — files outside it. That is a
/// containment property and not an optimisation, so it holds under both
/// [`Ignores`] settings.
pub fn files(root: &Path, ignores: Ignores) -> Walked {
    files_within(root, MAX_VISITED, ignores)
}

/// The walk itself, with the ceiling as an argument.
///
/// Split out for one reason and it is a test: proving the ceiling fires, and
/// that `truncated` is set when it does, otherwise costs a two-hundred-thousand
/// entry tree, and a guarantee that expensive to check is one nobody checks.
/// Production has exactly one caller and it passes [`MAX_VISITED`].
fn files_within(root: &Path, ceiling: usize, ignores: Ignores) -> Walked {
    let mut w = Walk {
        root: root.to_path_buf(),
        ceiling,
        visited: 0,
        out: Walked {
            files: Vec::new(),
            truncated: false,
            ignored: 0,
            ignored_names: Vec::new(),
            unreadable: Vec::new(),
        },
    };

    // The stack of ignore files in scope, outermost first. Seeded from the
    // ancestors of `root` because `path` may name a subdirectory: searching
    // `tools/` in a repository whose root `.gitignore` lists `target` must
    // still skip `tools/*/target`, exactly as `git` and `ripgrep` do.
    let mut stack = match ignores {
        Ignores::Walk => Vec::new(),
        Ignores::Honour => ancestor_ignores(root),
    };
    w.descend(&root.to_path_buf(), &mut stack, ignores);
    w.out
}

struct Walk {
    root: PathBuf,
    ceiling: usize,
    visited: usize,
    out: Walked,
}

impl Walk {
    /// One directory, then its subdirectories. An explicit method rather than a
    /// closure so the ignore stack can be pushed and popped around the
    /// recursion, which is what makes a nested `.gitignore` override the one
    /// above it.
    fn descend(&mut self, dir: &PathBuf, stack: &mut Vec<Gitignore>, ignores: Ignores) {
        if self.out.truncated {
            return;
        }

        // An unreadable subdirectory is skipped rather than failing the call:
        // "I could not open one directory" is not "the search failed", and
        // turning it into an error would make a single permission bit hide a
        // whole tree of results. **It is recorded, though.** Skipping silently
        // made `Grep` answer "no matches" about a file it never opened, which is
        // the one thing this crate refuses to do.
        let Ok(entries) = std::fs::read_dir(dir) else {
            self.out.unreadable.push(dir.clone());
            return;
        };

        let pushed = match ignores {
            Ignores::Walk => false,
            Ignores::Honour => match read_ignore_file(dir) {
                None => false,
                Some(gi) => {
                    stack.push(gi);
                    true
                }
            },
        };

        let mut subdirs: Vec<PathBuf> = Vec::new();
        for entry in entries.flatten() {
            // `visited` counts entries reached, directories and files alike,
            // which is the quantity the ceiling is meant to bound, because the
            // traversal is what costs and not the matching. Counting only
            // returned files would let a tree of a million empty directories
            // run unbounded while the counter stayed at zero.
            self.visited += 1;
            if self.visited > self.ceiling {
                self.out.truncated = true;
                break;
            }

            let path = entry.path();
            // `file_type` here does not follow links, so a symlinked directory
            // reports neither `is_dir` nor `is_file` and is therefore neither
            // descended into nor returned. That is the containment boundary.
            let Ok(ft) = entry.file_type() else {
                self.out.unreadable.push(path);
                continue;
            };
            let is_dir = ft.is_dir();

            if is_dir
                && entry
                    .file_name()
                    .to_str()
                    .is_some_and(|n| SKIP_DIRS.contains(&n))
            {
                continue;
            }

            if ignores == Ignores::Honour && is_ignored(stack, &path, is_dir) {
                self.out.ignored += 1;
                if self.out.ignored_names.len() < NAMED_IGNORES {
                    let rel = path.strip_prefix(&self.root).unwrap_or(&path);
                    self.out
                        .ignored_names
                        .push(rel.to_string_lossy().replace('\\', "/"));
                }
                continue;
            }

            if is_dir {
                subdirs.push(path);
            } else if ft.is_file() {
                self.out.files.push(path);
            }
        }

        for sub in subdirs {
            self.descend(&sub, stack, ignores);
            if self.out.truncated {
                break;
            }
        }

        if pushed {
            stack.pop();
        }
    }
}

/// Deepest ignore file wins, which is why the stack is scanned from the back.
/// Within one file the crate applies gitignore's own rule, last matching
/// pattern wins, so a `!` negation later in the same file re-includes.
fn is_ignored(stack: &[Gitignore], path: &Path, is_dir: bool) -> bool {
    for gi in stack.iter().rev() {
        match gi.matched(path, is_dir) {
            ignore::Match::Ignore(_) => return true,
            ignore::Match::Whitelist(_) => return false,
            ignore::Match::None => {}
        }
    }
    false
}

fn read_ignore_file(dir: &Path) -> Option<Gitignore> {
    let file = dir.join(".gitignore");
    if !file.is_file() {
        return None;
    }
    let mut b = GitignoreBuilder::new(dir);
    // A malformed line is the only error here and the crate has already kept
    // the lines it could parse. Refusing the whole file over one bad glob would
    // turn a typo in `.gitignore` into a walk through 255,000 build artefacts.
    let _ = b.add(&file);
    b.build().ok()
}

/// `.gitignore` files above `root`, outermost first, stopping at the repository
/// boundary. Without the stop this would climb into the home directory and
/// apply ignore files belonging to no repository at all.
fn ancestor_ignores(root: &Path) -> Vec<Gitignore> {
    let mut dirs: Vec<&Path> = Vec::new();
    let mut cur = root.parent();
    while let Some(d) = cur {
        dirs.push(d);
        if d.join(".git").exists() {
            break;
        }
        cur = d.parent();
    }
    // No repository above us means no repository-relative ignore files to
    // inherit, and climbing further would be reading a stranger's rules.
    if !dirs.last().is_some_and(|d| d.join(".git").exists()) {
        return Vec::new();
    }
    dirs.reverse();
    dirs.iter().filter_map(|d| read_ignore_file(d)).collect()
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

    fn write(path: &Path, body: &str) {
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p).expect("mkdir");
        }
        std::fs::write(path, body).expect("write");
    }

    /// The measured defect, in miniature. A large ignored directory sits in
    /// front of source files, the ceiling is low enough to be exhausted inside
    /// it, and the question is whether the source is still found.
    ///
    /// This is the test that fails on the old walk: it visited the build output
    /// first, hit the ceiling, and returned a short list that `Glob` reported
    /// as a complete count.
    #[test]
    fn a_large_ignored_directory_does_not_bury_the_source() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        write(&root.join(".gitignore"), "build/\n");
        for n in 0..200 {
            write(&root.join(format!("build/o{n}.rlib")), "x");
        }
        for n in 0..5 {
            write(&root.join(format!("src/m{n}.rs")), "x");
        }

        // A ceiling smaller than the ignored directory, which is the real
        // repository's shape: 255,636 entries under `target` against a 200,000
        // ceiling.
        let honoured = files_within(root, 50, Ignores::Honour);
        assert!(
            !honoured.truncated,
            "the ceiling fired on a tree of 6 unignored files"
        );
        assert_eq!(
            honoured.files.len(),
            6,
            "source went missing behind build output: {:?}",
            honoured.files
        );
        assert_eq!(honoured.ignored, 1, "the skip went uncounted");
        assert_eq!(honoured.ignored_names, vec!["build".to_string()]);

        // The same tree under the old behaviour, which `Ignores::Walk` still
        // reproduces exactly: nothing skipped but `.git`. This is the red half
        // of the fix, kept as an assertion rather than a memory. The ceiling is
        // exhausted inside build output and the source is never reached, which
        // is precisely the 27-of-107 result measured in Emma's own repository.
        //
        // The ignored directory is named `build/` so it sorts before `src/` on
        // Windows `read_dir` order — without that, the red half passes for the
        // wrong reason on this box.
        let walked = files_within(root, 50, Ignores::Walk);
        assert!(walked.truncated, "include_ignored must reach build output");
        assert_eq!(walked.ignored, 0);
        let reached_source = walked
            .files
            .iter()
            .filter(|p| p.to_string_lossy().contains("src/"))
            .count();
        assert!(
            reached_source < 5,
            "the fixture does not reproduce the defect: source was reached anyway"
        );
    }

    /// Deepest file wins and `!` re-includes. Stated as a test because the
    /// alternative to the crate was hand-rolled matching, and this is the pair
    /// of rules a hand-rolled version gets wrong.
    #[test]
    fn nested_ignore_files_and_negations_are_honoured() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        write(&root.join(".gitignore"), "*.log\nbuild/\n");
        write(&root.join("sub/.gitignore"), "!keep.log\n");
        write(&root.join("a.log"), "x");
        write(&root.join("sub/keep.log"), "x");
        write(&root.join("sub/drop.log"), "x");
        write(&root.join("build/out.bin"), "x");
        write(&root.join("src/main.rs"), "x");

        let w = files_within(root, MAX_VISITED, Ignores::Honour);
        let names: Vec<String> = w
            .files
            .iter()
            .map(|p| {
                p.strip_prefix(root)
                    .unwrap_or(p)
                    .to_string_lossy()
                    .replace('\\', "/")
            })
            .collect();
        assert!(names.contains(&"src/main.rs".to_string()), "{names:?}");
        assert!(
            names.contains(&"sub/keep.log".to_string()),
            "a nested negation was ignored: {names:?}"
        );
        assert!(!names.contains(&"a.log".to_string()), "{names:?}");
        assert!(!names.contains(&"sub/drop.log".to_string()), "{names:?}");
        assert!(!names.iter().any(|n| n.starts_with("build/")), "{names:?}");
        // The two `.gitignore` files are themselves tracked and returned, which
        // is the point: the ignore list is readable rather than compiled in.
        assert!(names.contains(&".gitignore".to_string()), "{names:?}");
    }

    /// A subdirectory search still honours the repository root's rules. Without
    /// this, `path=tools` would walk every `target` under it.
    #[test]
    fn a_subdirectory_search_inherits_the_repository_root_ignore() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        std::fs::create_dir_all(root.join(".git")).expect("mkdir");
        write(&root.join(".gitignore"), "target\n");
        write(&root.join("tools/fs/target/o.rlib"), "x");
        write(&root.join("tools/fs/src/lib.rs"), "x");

        let w = files_within(&root.join("tools"), MAX_VISITED, Ignores::Honour);
        assert_eq!(w.files.len(), 1, "{:?}", w.files);
        assert_eq!(w.ignored, 1);
    }

    /// `.git` is skipped whatever the setting, and an ignore file cannot
    /// re-include it. The object store is not a search result.
    #[test]
    fn the_object_store_is_skipped_under_both_settings() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        write(&root.join(".git/objects/ab/cdef"), "x");
        write(&root.join("a.rs"), "x");

        for mode in [Ignores::Honour, Ignores::Walk] {
            let w = files_within(root, MAX_VISITED, mode);
            assert_eq!(w.files.len(), 1, "{mode:?}: {:?}", w.files);
        }
    }

    /// Containment. A directory symlink pointing outside the root must not be
    /// descended into under either setting, because the escape hatch opens
    /// build output and not the rest of the machine.
    #[cfg(unix)]
    #[test]
    fn a_symlink_out_of_the_tree_is_never_followed() {
        let outside = tempfile::tempdir().expect("tempdir");
        write(&outside.path().join("secret.txt"), "x");
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        write(&root.join("a.rs"), "x");
        std::os::unix::fs::symlink(outside.path(), root.join("escape")).expect("symlink");

        for mode in [Ignores::Honour, Ignores::Walk] {
            let w = files_within(root, MAX_VISITED, mode);
            assert_eq!(
                w.files.len(),
                1,
                "{mode:?}: escaped the tree: {:?}",
                w.files
            );
        }
    }

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

        let whole = files_within(dir.path(), MAX_VISITED, Ignores::Honour);
        assert_eq!(whole.files.len(), 10);
        assert!(
            !whole.truncated,
            "a walk that finished must not claim a cut"
        );

        let cut = files_within(dir.path(), 3, Ignores::Honour);
        assert!(cut.files.len() < 10, "the ceiling did not bound anything");
        assert!(cut.truncated, "the walk stopped early and did not say so");
    }

    /// The sentence a cut walk hands to the model. It must name the number,
    /// admit the counts beside it are a floor, and offer a remedy that exists.
    /// The failure this whole mechanism was built after was advice to raise a
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

    /// A declared ignore list is only declared if the result declares it. The
    /// sentence has to carry the count, what was skipped, and the argument that
    /// turns the skipping off.
    #[test]
    fn the_ignore_notice_counts_names_and_offers_the_escape_hatch() {
        let s = ignored_notice(1, &["target".to_string()]);
        assert!(s.contains("1 paths ignored"), "{s}");
        assert!(s.contains("(target)"), "what was skipped went unnamed: {s}");
        assert!(s.contains("include_ignored=true"), "no escape hatch: {s}");

        let many = ignored_notice(
            9,
            &(0..NAMED_IGNORES)
                .map(|n| format!("d{n}"))
                .collect::<Vec<_>>(),
        );
        assert!(many.contains("and 3 more"), "the tail went unsaid: {many}");
    }

    /// Certified against the repository this crate lives in. Run with
    /// `cargo test -p emma-tools-fs --lib certify_gitignore -- --ignored --nocapture`.
    #[test]
    #[ignore = "walks the real tree; run explicitly for certification"]
    fn certify_gitignore_walk_against_this_repository() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .expect("repo root");
        let t0 = std::time::Instant::now();
        let honour = files_within(&root, MAX_VISITED, Ignores::Honour);
        let honour_ms = t0.elapsed().as_millis();
        let t1 = std::time::Instant::now();
        let walk = files_within(&root, MAX_VISITED, Ignores::Walk);
        let walk_ms = t1.elapsed().as_millis();
        eprintln!("Honour took {honour_ms} ms, Walk took {walk_ms} ms");
        eprintln!("Ignored names: {:?}", honour.ignored_names);
        // The measured defect in its original shape: `Glob **/*.md` returned 27
        // of 107 in the fork's tree. Counting markdown both ways here says what
        // the same call sees on this box.
        let md = |files: &[PathBuf]| {
            files
                .iter()
                .filter(|p| p.extension().is_some_and(|e| e == "md"))
                .count()
        };
        eprintln!(
            "markdown files: {} honouring, {} including ignored",
            md(&honour.files),
            md(&walk.files)
        );
        eprintln!(
            "Honour (default): {} files, {} ignored paths, truncated={}",
            honour.files.len(),
            honour.ignored,
            honour.truncated
        );
        eprintln!(
            "Walk (include_ignored): {} files, {} ignored paths, truncated={}",
            walk.files.len(),
            walk.ignored,
            walk.truncated
        );
        let target_in_honour = honour
            .files
            .iter()
            .filter(|p| {
                p.strip_prefix(&root)
                    .map(|r| {
                        r.to_string_lossy()
                            .replace('\\', "/")
                            .starts_with("target/")
                    })
                    .unwrap_or(false)
            })
            .count();
        eprintln!("Honour walk paths under target/: {target_in_honour}");
        assert!(honour.ignored > 0, "target/ must be skipped by default");
        assert_eq!(
            target_in_honour, 0,
            "target/ must not appear when honouring"
        );
        assert!(
            walk.files.len() > honour.files.len(),
            "include_ignored must reach more of the tree"
        );
    }
}
