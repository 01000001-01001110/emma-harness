//! No user-facing string prints a run of spaces it did not mean to.
//!
//! **This is a source assertion, and that is not the usual mistake.** This
//! repository has twice had a source grep standing in for a behavioural test —
//! `DEF-018`, then `DEF-045` one crate away — and both times the fix was to
//! stop reading the source and start reading the behaviour.
//!
//! The difference here is what the defect *is*. A Rust string literal wrapped
//! across two lines without a trailing `\` keeps the newline **and the next
//! line's indentation** as literal characters, so the source's shape becomes
//! the output's shape. The defect is a property of the text, so the text is the
//! right thing to read — and there is no runtime moment at which to catch it,
//! because every one of these messages renders correctly right up until the
//! rare branch that prints it.
//!
//! Found by a reviewer running the release binary: `emma model the API surface`
//! answered with 22 literal spaces mid-sentence, and `/export a b c` printed its
//! usage at 30 columns of indent. `DOC-001` had recorded this class as fixed —
//! its guard covers `Ending` variants only, so the class was alive in six other
//! files, including in code the rows under review had just added.
//!
//! **KNOWN GAP: this only sees the flattened, single-line spelling.**
//! `run_of_spaces` needs two quotes on one line, so a literal genuinely wrapped
//! across two source lines -- one quote on each -- is skipped before the space
//! scan runs. All ten instances found so far were flattened, which is why
//! nobody noticed; `cargo fmt` does not join string literals, so the two-line
//! spelling is stable and permanently invisible here. A reviewer found this by
//! reading the scanner against this doc.
//!
//! Four attempts at a cross-line scan were made and abandoned: a per-line
//! odd-quote test reports every *closing* line of a correctly continued literal
//! (40 false positives), and carrying the state properly means most of a Rust
//! lexer -- raw strings, char literals, comments, escapes -- inside a test. A
//! guard that cries wolf is worse than a stated gap, so the gap is stated. The
//! reviewer's own crude cross-line scan found 19 candidates and judged every one
//! a false positive, so this is a coverage hole rather than a second live
//! defect. *What would settle it:* a real lint (`clippy` lint or a `syn`-based
//! check) rather than a string scan in a test.

use std::path::{Path, PathBuf};

/// Deliberate column alignment, which is not this defect.
///
/// Test fixtures that draw a table on purpose. Listed by path rather than
/// detected, because "did the author mean this" is exactly the judgement a
/// heuristic gets wrong, and a silent allowance is worse than a named one.
///
/// **This list hid a defect within hours of being written, which is the
/// standing argument against escape hatches.** `agent_types.rs` was allowed here
/// under the reason "fixtures that stand in for a real store". One line in it
/// was not a fixture: an operator-facing `eprintln!` carrying a 14-space run,
/// added by `HARD-007` in the same review cycle that produced this guard, while
/// its sibling one file over was written correctly. A reviewer reimplemented the
/// scan, ran it over the three allowed files, and found exactly one reportable
/// run -- in the file whose whole justification was that it had none. The file
/// is no longer allowed and the line is fixed.
///
/// **A path goes in here only when every run in it has been looked at.** An
/// allowance whose author has not read the thing being allowed is a hole with a
/// comment on it.
const ALIGNED_ON_PURPOSE: &[&str] = &[
    // Sample reports for the tool pages: they are transcripts of a writer's
    // column layout, and the alignment is the content.
    "crates/emma/src/term/app.rs",
];

fn tracked_rust_files(root: &Path) -> Vec<PathBuf> {
    let out = std::process::Command::new("git")
        .args(["ls-files", "*.rs"])
        .current_dir(root)
        .output()
        .expect("git ls-files");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| root.join(l))
        .collect()
}

/// Without this, a wrapped literal reaches a user as a paragraph with a hole in
/// it, and nothing anywhere notices.
#[test]
fn no_user_facing_literal_prints_a_run_of_spaces() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root")
        .to_path_buf();

    let mut found: Vec<String> = Vec::new();
    for file in tracked_rust_files(&root) {
        let rel = file
            .strip_prefix(&root)
            .unwrap_or(&file)
            .to_string_lossy()
            .replace('\\', "/");
        if ALIGNED_ON_PURPOSE.iter().any(|a| rel == *a) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        for (n, line) in text.lines().enumerate() {
            let trimmed = line.trim_start();
            // Comments and doc comments describe; they do not print.
            if trimmed.starts_with("//") {
                continue;
            }
            // **Two thresholds, and the second is what separates the defect
            // from deliberate column alignment.** A padded label — `"model
            // {running}"`, `"cwd            {}"` — puts its run near the START
            // of the literal, because the label is short. A literal that wrapped
            // without a `\` puts its run wherever the author's editor ran out of
            // line, which is always far along. Every real instance found was at
            // column 87 or beyond; every deliberate one was inside column 40.
            //
            // A heuristic, and named as one. It fails toward reporting: a
            // deliberate alignment past column 60 costs one entry in the list
            // above with a reason, and a missed wrap costs a user a paragraph
            // with a hole in it.
            if let Some(col) = run_of_spaces(line, 6).filter(|c| *c >= 60) {
                found.push(format!("{rel}:{}: column {col}", n + 1));
            }
        }
    }

    assert!(
        found.is_empty(),
        "a string literal carries a run of 6+ spaces, which is what a line-wrapped \
         literal without a trailing `\\` produces. If the alignment is deliberate, add \
         the file to ALIGNED_ON_PURPOSE with a reason.\n{}",
        found.join("\n")
    );
}

/// The column of a run of `at_least` spaces that sits between two non-space
/// characters inside a double-quoted literal on this line.
///
/// Deliberately crude: it looks for a quote before and after, and does not
/// parse Rust. A raw string or a quote inside a comment could fool it, which is
/// why the assertion above names the file and line rather than trying to be
/// clever — a false positive costs one entry in the allow list and a false
/// negative costs a user a paragraph with a hole in it.
fn run_of_spaces(line: &str, at_least: usize) -> Option<usize> {
    let bytes = line.as_bytes();
    let first_quote = line.find('"')?;
    let last_quote = line.rfind('"')?;
    if last_quote <= first_quote {
        return None;
    }
    let mut i = first_quote + 1;
    while i < last_quote {
        if bytes[i] == b' ' {
            let start = i;
            while i < last_quote && bytes[i] == b' ' {
                i += 1;
            }
            let len = i - start;
            let before_is_text = start > first_quote + 1 && bytes[start - 1] != b' ';
            let after_is_text = i < last_quote && bytes[i] != b' ';
            if len >= at_least && before_is_text && after_is_text {
                return Some(start);
            }
        } else {
            i += 1;
        }
    }
    None
}
