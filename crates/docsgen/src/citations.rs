//! Every test name a docs page cites, checked against the tests that exist.
//!
//! **The site's credibility rests on its citations being real, and nothing was
//! checking them.** `docsgen` regenerates diagrams and three README blocks;
//! the prose around them — 494 quoted code blocks and 420 test citations
//! across 93 pages — was hand-maintained and hand-audited, which is to say
//! audited by whoever remembered. On 2026-09-07 a sweep found ten dead
//! citations, two of them created that same day: a test the duplication sweep
//! deleted, and one renamed when the tool surface grew from four to seven.
//! Neither was noticed. In the same session an author wrote two test names
//! that had never existed at all.
//!
//! A dead citation is worse than a missing one. `<code>a_thing_is_true</code>`
//! beside a `tested` chip is a promise that somebody can go and read the proof,
//! and a reader who cannot find it has no way to tell a renamed test from a
//! guarantee that was quietly dropped.
//!
//! **A name a page deliberately reports as missing is written in `<em>`, not
//! `<code>`.** Several pages record a phantom -- a test somebody once cited that
//! never existed, or a guarantee whose test was deleted -- and say so in the
//! prose around it. To this check those look exactly like a dead citation,
//! because they are the same string in the same tag. The convention is the
//! distinction: `<code>` is a pointer a reader can follow, `<em>` is a name
//! being talked *about*. The first sweep under this gate nearly turned three of
//! those into false receipts by "fixing" them to point at real tests that
//! prove something else.
//!
//! **What this checks and what it deliberately does not.** It matches
//! test-*shaped* names — the naming convention this project actually uses, a
//! sentence in snake_case — against every `fn` in the workspace. It does not
//! check that the test proves what the sentence beside it claims; no mechanical
//! check can, and pretending otherwise would be its own false receipt. It
//! narrows the hand-audit to the part a human has to do.

use std::collections::BTreeSet;
use std::path::Path;

use anyhow::{Context, Result};

/// The prefixes a test name starts with in this project. Names read as
/// sentences — `a_burst_of_edits_collapses_into_one_did_change` — so the
/// opening word is what distinguishes a citation of a *test* from a citation
/// of a function, a field, or a wire key like `content_block_delta`.
///
/// Narrow on purpose. A prefix list that caught every identifier would flag
/// struct fields and JSON keys, and a check that cries wolf is one somebody
/// switches off.
const TEST_PREFIXES: [&str; 6] = ["a_", "an_", "the_", "no_", "every_", "nothing_"];

/// A citation that names no test in the workspace.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Dead {
    /// The page that cites it, as a file name.
    pub page: String,
    /// The name it cites.
    pub name: String,
}

impl std::fmt::Display for Dead {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} cites {}, which no test defines",
            self.page, self.name
        )
    }
}

/// Every test-shaped `<code>` citation under `docs/` that names no `fn`.
pub fn dead_citations(root: &Path) -> Result<Vec<Dead>> {
    let defined = defined_fns(root)?;
    let docs = root.join("docs");
    let mut out = Vec::new();
    let entries = std::fs::read_dir(&docs)
        .with_context(|| format!("reading {}", docs.display()))?
        .filter_map(Result::ok);
    for entry in entries {
        let path = entry.path();
        if path.extension().is_none_or(|e| e != "html") {
            continue;
        }
        let page = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        for name in cited(&text) {
            if !defined.contains(&name) {
                out.push(Dead {
                    page: page.clone(),
                    name,
                });
            }
        }
    }
    out.sort();
    out.dedup();
    Ok(out)
}

/// The test-shaped names a page cites inside `<code>` tags.
///
/// A plain scan rather than an HTML parse: the pages are hand-written and the
/// tag is always spelled `<code>`, so a parser would be a dependency bought to
/// handle markup nobody writes here.
fn cited(page: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for chunk in page.split("<code>").skip(1) {
        let Some(inner) = chunk.split_once("</code>") else {
            continue;
        };
        let name = inner.0.trim();
        if is_test_shaped(name) {
            out.insert(name.to_string());
        }
    }
    out
}

/// Whether a citation is a test name rather than a field, a key or a path.
///
/// Length is part of it: this project's test names are sentences, and a short
/// `a_name` is far more likely to be a variable somebody quoted.
fn is_test_shaped(name: &str) -> bool {
    if name.len() < 16 || name.len() > 120 {
        return false;
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
    {
        return false;
    }
    // At least three words, so `a_long_identifier` from a struct does not
    // read as a sentence by length alone.
    if name.matches('_').count() < 3 {
        return false;
    }
    TEST_PREFIXES.iter().any(|p| name.starts_with(p))
}

/// Every `fn` name defined anywhere in the workspace's Rust sources.
///
/// Every one, not only those inside a `#[cfg(test)]` module: a test helper and
/// a test are both legitimate things for a page to name, and the question here
/// is only whether the reader can find what the page points at.
fn defined_fns(root: &Path) -> Result<BTreeSet<String>> {
    let mut out = BTreeSet::new();
    let mut stack = vec![root.join("crates"), root.join("tools")];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.filter_map(Result::ok) {
            let path = entry.path();
            if path.is_dir() {
                // `target` holds build output, including vendored sources whose
                // function names are not this project's to cite.
                if path.file_name().is_some_and(|n| n == "target") {
                    continue;
                }
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                let text = std::fs::read_to_string(&path)
                    .with_context(|| format!("reading {}", path.display()))?;
                collect_fns(&text, &mut out);
            }
        }
    }
    Ok(out)
}

/// Pull `fn <name>` out of a source file.
fn collect_fns(text: &str, out: &mut BTreeSet<String>) {
    for chunk in text.split("fn ").skip(1) {
        let name: String = chunk
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect();
        if !name.is_empty() {
            out.insert(name);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sentence_shaped_name_is_a_citation_and_a_field_name_is_not() {
        assert!(is_test_shaped("a_burst_of_edits_collapses_into_one"));
        assert!(is_test_shaped("the_surface_is_the_seven_tools"));
        assert!(is_test_shaped("nothing_machine_specific_reaches_it"));
        // Real things quoted on these pages that are not tests.
        assert!(!is_test_shaped("completion_triggers"), "a struct field");
        assert!(!is_test_shaped("content_block_delta"), "a wire key");
        assert!(!is_test_shaped("cache_read_input_tokens"), "a JSON key");
        assert!(!is_test_shaped("a_b_c"), "too short to be a sentence");
        assert!(!is_test_shaped("ce26c5f39467e4f0"), "a hash");
        assert!(!is_test_shaped("Some_Thing_Here"), "not snake_case");
    }

    #[test]
    fn a_citation_is_read_out_of_a_code_tag_and_prose_is_left_alone() {
        let page = "<p>proved by <code>a_thing_that_is_true_here</code> and \
                    a_thing_not_in_code_tags is prose</p>";
        let found = cited(page);
        assert!(found.contains("a_thing_that_is_true_here"));
        assert_eq!(found.len(), 1, "{found:?}");
    }

    #[test]
    fn a_defined_fn_is_found_however_it_is_declared() {
        let mut out = BTreeSet::new();
        collect_fns(
            "pub fn one() {}\n  async fn two() {}\n#[test]\nfn three_things_here() {}",
            &mut out,
        );
        assert!(out.contains("one") && out.contains("two") && out.contains("three_things_here"));
    }
}
