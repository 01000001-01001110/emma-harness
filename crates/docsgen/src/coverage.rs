//! Which source modules no page under `docs/` names.
//!
//! # Why this exists
//!
//! `docs/` generates every diagram from source, and a test fails when a
//! committed diagram drifts from the code it was drawn from. That machinery
//! says nothing about the prose next to the diagram. Two wrong claims shipped
//! because of exactly that gap: a page kept asserting a second provider was
//! "planned and not built" long after one shipped, and the module that talks
//! to a third-party API was cited on no page at all, so nobody reading the
//! docs had a place to go correct a wrong answer about it.
//!
//! The check this module runs is the cheap version of "somebody read this
//! module before writing about the system it belongs to": does its path
//! appear on any page under `docs/`? A hit is not proof the prose is right --
//! a stale sentence can sit next to an accurate diagram forever. A miss is
//! stronger: it means no page so much as names the file, so no prose owns
//! being wrong about it. `roster.rs` and `ollama.rs`, the two newest
//! provider-side modules, were both misses when this was written.
//!
//! # Why a report and not a gate
//!
//! 48 of 131 modules are uncited today. A `cargo test` gate that fails on that
//! count is a gate somebody silences on day one, and a silenced gate stays
//! silenced. This is data a caller can print, diff over time, or fold into a
//! gate later once the number is small enough that a new miss is worth
//! stopping a build for. Nothing here calls `panic!` or returns an empty
//! `Ok(())` on a miss.

use std::path::Path;

use anyhow::{Context, Result};

/// One source file under `crates/*/src` or `tools/*/src`, and whether any
/// page under `docs/` mentions its path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Module {
    /// Repo-relative, forward-slashed: `crates/llm/src/roster.rs`. This is
    /// also the exact string a docs page has to contain for a hit -- the
    /// same spelling `architecture.rs` already reads out of `Cargo.toml` and
    /// the same one a person would paste into a sentence.
    pub path: String,
    /// True when some page under `docs/*.html` contains `path` verbatim.
    pub cited: bool,
}

/// Every `.rs` file under `crates/*/src` or `tools/*/src`, sorted by path.
///
/// Excludes anything under a `tests/` directory (integration tests document
/// behaviour, not a module the system is built from) and anything under
/// `target/` (build output, not source -- and `cargo test` can leave a
/// `target/` next to a crate that itself contains a nested `src/` from a
/// dependency's source tarball, which would otherwise get walked twice: once
/// as workspace source and once as somebody else's).
pub fn source_modules(root: &Path) -> Result<Vec<String>> {
    let mut out = Vec::new();
    for group in ["crates", "tools"] {
        let dir = root.join(group);
        if !dir.is_dir() {
            continue;
        }
        for entry in std::fs::read_dir(&dir)
            .with_context(|| format!("{} is a workspace group directory", dir.display()))?
        {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let src = entry.path().join("src");
            if src.is_dir() {
                walk(root, &src, &mut out)?;
            }
        }
    }
    out.sort();
    Ok(out)
}

/// Recurse under `dir`, appending every `.rs` file's repo-relative,
/// forward-slashed path to `out` -- skipping `target/` and `tests/`
/// subdirectories entirely rather than filtering their contents out
/// afterwards, so a huge `target/` under a crate's `src/` (unusual, but nothing
/// stops a build tool from putting one there) never gets opened at all.
fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) -> Result<()> {
    for entry in
        std::fs::read_dir(dir).with_context(|| format!("{} is walked for source", dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            let name = entry.file_name();
            if name == "target" || name == "tests" {
                continue;
            }
            walk(root, &path, out)?;
        } else if file_type.is_file() && path.extension().is_some_and(|e| e == "rs") {
            out.push(repo_relative(root, &path));
        }
    }
    Ok(())
}

/// `path`, made relative to `root` and forward-slashed.
///
/// Forward-slashed because that is the spelling every docs page under this
/// repo uses regardless of what platform wrote it, and a backslash from
/// `Path::display` on Windows would make every module on this platform read
/// as uncited even where the citation is right there.
fn repo_relative(root: &Path, path: &Path) -> String {
    let rel = path.strip_prefix(root).unwrap_or(path);
    rel.components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

/// Every `docs/*.html` page's text, concatenated. Callers only ever ask
/// "does this substring occur anywhere", so one string spares building a
/// per-page structure nobody needs.
fn docs_text(root: &Path) -> Result<String> {
    let dir = root.join("docs");
    let mut text = String::new();
    for entry in std::fs::read_dir(&dir)
        .with_context(|| format!("{} holds the docs pages", dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "html") {
            text.push_str(
                &std::fs::read_to_string(&path)
                    .with_context(|| format!("{} is a docs page", path.display()))?,
            );
            // A separator, so a path split across the end of one page and the
            // start of the next (impossible today, cheap to rule out) can
            // never falsely match.
            text.push('\n');
        }
    }
    Ok(text)
}

/// Every source module together with whether some page under `docs/` cites
/// its path.
pub fn coverage(root: &Path) -> Result<Vec<Module>> {
    let modules = source_modules(root)?;
    let text = docs_text(root)?;
    Ok(modules
        .into_iter()
        .map(|path| {
            let cited = text.contains(&path);
            Module { path, cited }
        })
        .collect())
}

/// The paths of modules no page under `docs/` cites, sorted.
///
/// This is the function a caller wants for the report: `coverage` exists so a
/// test can also see the cited side, but nobody printing a punch list wants
/// the 83 modules that already have a home.
pub fn uncited(root: &Path) -> Result<Vec<String>> {
    Ok(coverage(root)?
        .into_iter()
        .filter(|m| !m.cited)
        .map(|m| m.path)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn workspace_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .expect("the workspace root exists")
    }

    /// **If this breaks:** the walker stopped finding real source, or started
    /// finding something that is not source (a `target/` directory, a test
    /// file). Not an exact count -- the workspace grows, and a test asserting
    /// "131 modules" fails on every commit that adds a file, which is the
    /// wrong reason for this test to go red. 131 modules exist as of this
    /// writing (`docs/coverage.html` does not exist yet, precisely because
    /// this is a report and not a gate).
    #[test]
    fn the_walker_finds_a_plausible_number_of_modules() {
        let modules = source_modules(&workspace_root()).expect("workspace root reads");
        assert!(
            modules.len() > 50,
            "found only {}, the walker likely stopped early",
            modules.len()
        );
        assert!(
            modules.len() < 1000,
            "found {}, the walker likely descended into target/ or a dependency",
            modules.len()
        );
    }

    /// **If this breaks:** either `agent.rs` stopped being cited by any docs
    /// page (in which case the fact this guards is no longer true and the
    /// test should change), or `coverage` stopped recognising a citation that
    /// is plainly there -- confirmed by `grep -l crates/emma/src/agent.rs
    /// docs/*.html`, which matches `docs/consent-exempt.html` and others.
    #[test]
    fn a_module_known_to_be_cited_is_reported_as_cited() {
        let modules = coverage(&workspace_root()).expect("workspace root reads");
        let agent = modules
            .iter()
            .find(|m| m.path == "crates/emma/src/agent.rs")
            .expect("agent.rs is a workspace source file");
        assert!(
            agent.cited,
            "agent.rs is cited on docs/consent-exempt.html and others"
        );
    }

    /// **If this breaks:** either `theme.rs` gained a citation (good news --
    /// update the module this test names to a module that is still uncited),
    /// or `coverage` stopped seeing a real gap.
    ///
    /// This named `crates/llm/src/roster.rs` when it was written, and that
    /// module was cited the same afternoon by the pass this report was built
    /// to drive, which is the outcome the test exists to notice. The generator's
    /// own theme file is the fixture now: `docs/` describes Emma, not the tool
    /// that draws its diagrams, so this one should stay uncited for as long as
    /// that division holds.
    #[test]
    fn a_module_known_to_be_uncited_is_reported_as_uncited() {
        let modules = coverage(&workspace_root()).expect("workspace root reads");
        let theme = modules
            .iter()
            .find(|m| m.path == "crates/docsgen/src/theme.rs")
            .expect("theme.rs is a workspace source file");
        assert!(
            !theme.cited,
            "theme.rs was uncited when this test was written; if it now \
             appears on a docs page this assertion should move to a module \
             that is still a gap"
        );
    }

    /// **If this breaks:** `uncited` stopped being the complement of the
    /// cited set, which means the two functions disagree about what a
    /// citation is.
    #[test]
    fn uncited_is_exactly_the_modules_coverage_marks_as_not_cited() {
        let root = workspace_root();
        let all = coverage(&root).expect("workspace root reads");
        let gaps = uncited(&root).expect("workspace root reads");
        let expected: Vec<String> = all
            .into_iter()
            .filter(|m| !m.cited)
            .map(|m| m.path)
            .collect();
        assert_eq!(gaps, expected);
    }
}
