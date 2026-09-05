//! `.platform/context.yaml` — what this **repository** is, detected
//! mechanically.
//!
//! Ported from the Mac branch, per the divergent-fork audit reviewed on
//! 2026-08-27. **No model call anywhere
//! in here**: every field is something the process looked up in a manifest, a
//! README or a directory entry, and a field that would have to be guessed is
//! absent rather than inferred. A curated fence in the file survives a
//! refresh, so a human's notes are not overwritten by the next detection run.
//!
//! # It is not about the machine, and that is deliberate
//!
//! The stub this replaced said "what this machine is". The branch's code
//! never detected a machine and neither does this: `os`, `arch`, a home
//! directory, an installed toolchain — none of it is here. The reason is
//! where the file lands. `.platform/context.yaml` sits **inside the project
//! directory** and is meant to be committed, so a machine fact written into
//! it is a fact about whoever ran `emma init` last, presented to everybody
//! else as a fact about the project. It would also flip on every teammate's
//! refresh, which is diff churn over a claim that was wrong for every reader
//! but one. What is on *this* machine is [`crate::usertools`]'s question, it
//! is answered per-process and never written down, and there is no second
//! answer here.
//!
//! # What this is for
//!
//! A model that opens a repository pays for orientation before it does any
//! work: an `ls`, a `README`, three `Grep`s that find the wrong file, and on
//! a small model that budget is most of what it has. This file is the answer
//! written down once: what the repository is, what it is written in, where it
//! starts, what its directories are for, how it is conventionally built and
//! tested, and where the long version lives. It is meant to be read whole, so
//! it stays short.
//!
//! **It is only worth that if it is current.** A stale map is worse than no
//! map: the reader that trusts it does not check it. So the file carries its
//! own contract in its own comments, and the mechanical half of it is cheap
//! to regenerate — `emma init` again, no model call, no network.
//!
//! # The two zones
//!
//! Everything above the curated fence is **derived** and is rewritten on
//! every refresh. Everything from the fence down is **written by a person, or
//! by a model that learned something**, and this module never touches it. Two
//! zones rather than one merged document because merging is how you lose a
//! sentence somebody wrote, and because the derived half must be allowed to
//! be wrong and thrown away.
//!
//! The seam is a comment line, and a file that has lost it is refused rather
//! than rewritten. So is a file that cannot be read at all — not valid UTF-8,
//! a directory, no permission. **That second refusal is a change from the
//! branch**, where any read error fell through to the create path and
//! overwrote the curated zone with a fresh empty one; a file somebody saved
//! as UTF-16 in Notepad was one `emma init` away from losing its prose.
//!
//! The write is to a sibling temporary file followed by a rename, for the
//! same reason: the curated zone is often the only copy of what it holds, and
//! a truncated `write` at the wrong moment loses it.
//!
//! # Detection is mechanical, and admits what it does not know
//!
//! Manifests, file existence, directory names, the README's first paragraph.
//! No full TOML parser: the reader here takes the handful of keys it needs
//! off the top-level tables and **omits a key it cannot read** rather than
//! reporting a value it inferred. An empty `languages:` is a different claim
//! from an absent one, and the difference is the point.
//!
//! Three fields are weaker than the rest, and each says so **in the file it
//! writes** rather than only here, because the file outlives the doc:
//!
//! - `layout[].role` is read off the directory's *name* against a fixed table
//!   of conventions. Nothing looked inside. The two exceptions —
//!   `cargo crate`, `cargo workspace members` — are a `Cargo.toml` seen on
//!   disk.
//! - `commands` are the language's conventional invocations, emitted because
//!   a manifest for that language is present. `cargo build` was not read out
//!   of `Cargo.toml` and nothing checked that clippy is installed. The npm
//!   lines are the exception: those script names were read from
//!   `package.json`.
//! - `repo.name` falls back to the checkout's directory name when no manifest
//!   names the project, which differs between two people's clones.
//!
//! # It is plaintext, in the project, and meant to be committed
//!
//! Nothing here encrypts anything. **Do not put a credential below the
//! fence**: this file is in every clone the moment it is committed, and
//! `git rm` does not take it out of history. It should *not* be gitignored —
//! a map only helps the stranger who has not run `emma init` yet, and the
//! curated prose is worth versioning — and the rule that keeps that safe is
//! the one at the top: no machine facts, no paths outside the repository, no
//! secrets. Everything the mechanical zone writes is already in the
//! repository somewhere else.

use std::collections::BTreeMap;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::memory::clock;

/// The directory. Named for the thing rather than for Emma: a second tool that
/// wants to read this file should not have to know which agent wrote it.
pub const DIR: &str = ".platform";
/// The file inside [`DIR`].
pub const FILE: &str = "context.yaml";

/// The seam. Everything from this line to the end of the file survives a
/// refresh untouched, byte for byte.
const CURATED_FENCE: &str =
    "# --- maintained: curated. `emma init` never rewrites anything below this line ---";

/// Where the wiki is, relative to the repository. One spelling, used in the
/// YAML and in the prose. Kept in step with `memory::Wiki::project`, which
/// opens `<repo>/.emma/memory/`.
const MEMORY_INDEX: &str = ".emma/memory/index.md";
const MEMORY_SCHEMA: &str = ".emma/memory/schema.md";

/// Where the context file lives for a repository rooted at `cwd`.
pub fn path(cwd: &Path) -> PathBuf {
    cwd.join(DIR).join(FILE)
}

// region: the survey
// ---------------------------------------------------------------------------
// What can be known without asking anybody
// ---------------------------------------------------------------------------

/// A file a reader should open first, and what kind of thing it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub path: String,
    pub kind: &'static str,
}

/// A top-level directory and, when the name is one this knows, what it holds.
/// `role` is `None` for a directory nobody can describe mechanically, and that
/// is a gap for the curated zone, not a licence to invent a sentence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dir {
    pub path: String,
    pub role: Option<String>,
}

/// Everything the file's derived zone is built from.
///
/// `name` is an `Option` rather than a string with a fallback: a path with no
/// final component — a filesystem root — has no name, and writing the word
/// `repository` there would be a value nobody supplied sitting in the slot a
/// reader trusts.
#[derive(Debug, Clone, Default)]
pub struct Survey {
    pub name: Option<String>,
    pub purpose: Option<String>,
    pub languages: Vec<&'static str>,
    /// `("rust", [("edition", "2021")])`. Flat, ordered, and only keys that
    /// were actually read out of a manifest.
    pub toolchain: BTreeMap<&'static str, Vec<(&'static str, String)>>,
    pub entry_points: Vec<Entry>,
    pub layout: Vec<Dir>,
    pub commands: Vec<(&'static str, String)>,
}

/// Directories excluded from the map: build output and version control. They
/// are noise in every repository and their absence is not a claim.
const IGNORED: &[&str] = &[
    "target",
    "node_modules",
    "dist",
    "build",
    "vendor",
    "__pycache__",
    ".git",
];

/// Names whose meaning is a convention rather than a guess.
///
/// This is still a claim about a *name*, not about contents — a `docs/` full
/// of Perl reads as "documentation" here. The file says so where it prints
/// the role, so the reader can discount it.
fn known_role(name: &str) -> Option<&'static str> {
    Some(match name {
        "src" => "source",
        "tests" | "test" => "tests",
        "docs" | "doc" => "documentation",
        "examples" => "examples",
        "benches" | "benchmarks" => "benchmarks",
        "scripts" | "bin" => "scripts",
        "assets" | "static" | "public" => "static assets",
        "migrations" => "database migrations",
        "notes" => "working notes",
        _ => return None,
    })
}

/// The checkout's own directory name, or `None` at a filesystem root.
fn dir_name(cwd: &Path) -> Option<String> {
    cwd.file_name().map(|n| n.to_string_lossy().into_owned())
}

/// Read the repository. Nothing here fails: a manifest that cannot be read
/// leaves its keys out.
pub fn survey(cwd: &Path) -> Survey {
    let mut s = Survey {
        name: dir_name(cwd),
        ..Survey::default()
    };

    let cargo = cwd.join("Cargo.toml");
    if cargo.is_file() {
        rust(cwd, &cargo, &mut s);
    }
    let package = cwd.join("package.json");
    if package.is_file() {
        node(cwd, &package, &mut s);
    }
    let pyproject = cwd.join("pyproject.toml");
    if pyproject.is_file() {
        python(cwd, &pyproject, &mut s);
    }
    if cwd.join("go.mod").is_file() {
        go(cwd, &mut s);
    }

    // The README wins over a manifest description: it is the line the project's
    // own author wrote for a human.
    if let Some(line) = readme_purpose(cwd) {
        s.purpose = Some(line);
    }

    s.layout = layout(cwd);
    s
}

fn layout(cwd: &Path) -> Vec<Dir> {
    let Ok(read) = fs::read_dir(cwd) else {
        return Vec::new();
    };
    let excluded = gitignored_dirs(cwd);
    let names = read
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| !n.starts_with('.') && !IGNORED.contains(&n.as_str()) && !excluded.contains(n))
        .collect();
    let mut dirs = ordered(names);
    for d in &mut dirs {
        if d.role.is_none() && cwd.join(&d.path).join("Cargo.toml").is_file() {
            d.role = Some("cargo crate".into());
        } else if d.role.is_none() && holds_crates(&cwd.join(&d.path)) {
            d.role = Some("cargo workspace members".into());
        }
    }
    dirs
}

/// Top-level directory names the repository's own `.gitignore` excludes.
///
/// **Found by certifying against this repository rather than a fixture.** The
/// map of Emma's own tree listed `memory/` — the hook scratch directory
/// `.gitignore` excludes — as part of the repository's shape, next to `crates`
/// and `docs`. A directory git does not track is not the repository, and a map
/// that says otherwise sends a reader somewhere that is empty on their clone.
///
/// Read conservatively: a line that is a plain name, with an optional leading
/// or trailing `/`. Anything with a wildcard, a `!` negation or an interior
/// slash is left alone. Getting gitignore's precedence rules wrong in the
/// dropping direction hides a real directory, which is worse than the litter,
/// and this is not the place to reimplement `git check-ignore`.
fn gitignored_dirs(cwd: &Path) -> Vec<String> {
    let Ok(text) = fs::read_to_string(cwd.join(".gitignore")) else {
        return Vec::new();
    };
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#') && !l.starts_with('!'))
        .filter_map(|l| {
            let name = l.trim_start_matches('/').trim_end_matches('/');
            (!name.is_empty() && !name.contains('/') && !name.contains(['*', '?', '[']))
                .then(|| name.to_string())
        })
        .collect()
}

/// Directory names into `Dir`s, in name order.
///
/// **Split out of [`layout`] so the ordering can be tested at all.** `read_dir`
/// hands entries back in whatever order the filesystem keeps them, and on NTFS
/// — the only filesystem this repository's tests have run on — that order is
/// already by name. So a test that builds a directory and checks `layout`'s
/// output is sorted passes with the sort deleted: it is an assertion the world
/// already satisfies, which is the defect class this project calls a false
/// receipt. Sorting a list somebody handed over in the wrong order is a claim
/// the world does not make for us.
///
/// Why it must be sorted at all: two runs on one tree have to produce one
/// file, or a committed context file shows a diff on the next contributor's
/// machine for no reason anybody can read.
fn ordered(mut names: Vec<String>) -> Vec<Dir> {
    names.sort();
    names
        .into_iter()
        .map(|name| Dir {
            role: known_role(&name).map(str::to_string),
            path: name,
        })
        .collect()
}

fn holds_crates(dir: &Path) -> bool {
    fs::read_dir(dir).is_ok_and(|read| {
        read.flatten()
            .any(|e| e.path().join("Cargo.toml").is_file())
    })
}

/// The longest a `purpose:` may be before it is cut at a sentence boundary.
const PURPOSE_MAX: usize = 240;

/// The README's opening sentence, with the title heading skipped.
///
/// Skipped as well: badge lines and HTML, which are the two things that sit
/// between a title and the first sentence and neither of which says what the
/// project is.
///
/// **The paragraph is joined before the sentence is cut.** A README is hard
/// wrapped, so the first *line* is most of a sentence and reads as a
/// truncation. That is how the first version of this shipped, and the value it
/// produced ended mid-clause.
fn readme_purpose(cwd: &Path) -> Option<String> {
    let text = ["README.md", "README", "readme.md", "README.txt"]
        .iter()
        .find_map(|n| fs::read_to_string(cwd.join(n)).ok())?;
    let prose = |l: &&str| {
        let l = l.trim();
        !l.is_empty()
            && !l.starts_with('#')
            && !l.starts_with('<')
            && !l.starts_with("[!")
            && !l.starts_with("![")
            && !l.starts_with("```")
            && !l.starts_with('>')
            && !l.starts_with('|')
    };
    let mut lines = text.lines().skip_while(|l| !prose(l));
    let mut para = String::new();
    for line in lines.by_ref() {
        if line.trim().is_empty() {
            break;
        }
        if !para.is_empty() {
            para.push(' ');
        }
        para.push_str(line.trim());
    }
    if para.is_empty() {
        return None;
    }
    Some(first_sentences(&para, PURPOSE_MAX))
}

/// The paragraph, or as many whole sentences of it as fit. Cutting at a
/// sentence boundary rather than at a character count: a purpose that ends
/// mid-word is a purpose a reader has to go and check, which is the cost this
/// file exists to remove.
fn first_sentences(para: &str, max: usize) -> String {
    if para.chars().count() <= max {
        return para.to_string();
    }
    let head: String = para.chars().take(max).collect();
    match head.rfind(". ") {
        Some(at) => head[..=at].trim().to_string(),
        // No sentence ended in range: cut at a word instead, and say so with an
        // ellipsis rather than pretending the sentence finished.
        None => match head.rfind(' ') {
            Some(at) => format!("{}…", head[..at].trim()),
            None => head,
        },
    }
}

fn rust(cwd: &Path, manifest: &Path, s: &mut Survey) {
    let Ok(text) = fs::read_to_string(manifest) else {
        return;
    };
    s.languages.push("rust");
    if let Some(name) = toml_string(&text, "[package]", "name") {
        s.name = Some(name);
    }
    let mut keys = Vec::new();
    for table in ["[package]", "[workspace.package]"] {
        for key in ["edition", "rust-version"] {
            if let Some(v) = toml_string(&text, table, key) {
                if !keys.iter().any(|(k, _)| *k == key) {
                    keys.push((key, v));
                }
            }
        }
    }
    if !keys.is_empty() {
        s.toolchain.insert("rust", keys);
    }

    let has_bin = cwd.join("src").join("main.rs").is_file();
    if has_bin {
        s.entry_points.push(Entry {
            path: "src/main.rs".into(),
            kind: "binary",
        });
    }
    if cwd.join("src").join("lib.rs").is_file() {
        s.entry_points.push(Entry {
            path: "src/lib.rs".into(),
            kind: "library",
        });
    }
    // A workspace: every member's own entry point, so the map is of the
    // repository rather than of its root crate.
    for member in workspace_members(cwd) {
        for (file, kind) in [("main.rs", "binary"), ("lib.rs", "library")] {
            let rel = format!("{member}/src/{file}");
            if cwd.join(&rel).is_file() {
                s.entry_points.push(Entry { path: rel, kind });
            }
        }
    }
    s.commands.push(("build", "cargo build".into()));
    s.commands.push(("test", "cargo test".into()));
    // `cargo run` is only the right spelling when cargo has one binary to pick.
    // In a workspace it needs `-p`, and the package name has to come out of the
    // member's own manifest rather than from its directory name — those differ
    // here, where `crates/emma` is the `emma` package and `tools/fs` is
    // `emma-tools-fs`. If there is more than one binary, no `run:` line is
    // written at all: a command that does not run is worse than an absent one.
    if has_bin {
        s.commands.push(("run", "cargo run".into()));
    } else if let [only] = binary_members(cwd).as_slice() {
        s.commands.push(("run", format!("cargo run -p {only}")));
    }
    s.commands
        .push(("lint", "cargo clippy --all-targets".into()));
}

/// Member directories, found on the filesystem rather than parsed out of the
/// `members` array: a glob in that array is a pattern this would have to
/// implement, and the directories are right there.
///
/// Two levels only — `crates/emma`, not `a/b/c` — because that is the shape
/// every workspace in the wild uses and an unbounded walk of a repository is
/// a cost this file is supposed to save rather than spend.
fn workspace_members(cwd: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let Ok(read) = fs::read_dir(cwd) else {
        return out;
    };
    let mut tops: Vec<PathBuf> = read
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.is_dir()
                && p.file_name()
                    .is_some_and(|n| !n.to_string_lossy().starts_with('.'))
                && !IGNORED.contains(&&*p.file_name().unwrap_or_default().to_string_lossy())
        })
        .collect();
    tops.sort();
    for top in tops {
        let name = top.file_name().unwrap_or_default().to_string_lossy();
        if top.join("Cargo.toml").is_file() {
            out.push(name.into_owned());
            continue;
        }
        let Ok(inner) = fs::read_dir(&top) else {
            continue;
        };
        let mut kids: Vec<PathBuf> = inner
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.join("Cargo.toml").is_file())
            .collect();
        kids.sort();
        for kid in kids {
            out.push(format!(
                "{name}/{}",
                kid.file_name().unwrap_or_default().to_string_lossy()
            ));
        }
    }
    out
}

/// The package names of the workspace members that build a binary.
fn binary_members(cwd: &Path) -> Vec<String> {
    workspace_members(cwd)
        .into_iter()
        .filter(|m| cwd.join(m).join("src").join("main.rs").is_file())
        .filter_map(|m| {
            let text = fs::read_to_string(cwd.join(&m).join("Cargo.toml")).ok()?;
            toml_string(&text, "[package]", "name")
        })
        .collect()
}

/// One key out of one top-level table, without a TOML parser.
///
/// It reads the lines between `table` and the next `[`, and takes the first
/// `key = value`. That covers a manifest written the ordinary way and returns
/// `None` for anything else — a multi-line value, an inline table, a key
/// spelled `version.workspace`. `None` is a key left out of the context file,
/// which is the safe direction: the alternative is publishing a value nobody
/// wrote.
fn toml_string(text: &str, table: &str, key: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .skip_while(|l| *l != table)
        .skip(1)
        .take_while(|l| !l.starts_with('['))
        .find_map(|l| {
            let (k, v) = l.split_once('=')?;
            (k.trim() == key).then_some(v)
        })
        .and_then(toml_scalar)
}

/// The value half of `key = value`, or `None` when it is not a scalar this
/// reads.
///
/// The branch's version was `v.trim().trim_matches('"')`, which is wrong on
/// the single most common decoration a manifest carries: `edition = "2021" #
/// bump me` came back as `2021" # bump me` and went into the file as if it
/// had been read. Stripping delimiters is not parsing — the quote has to end
/// the value, and what follows it has to be discarded.
fn toml_scalar(v: &str) -> Option<String> {
    let v = v.trim();
    if let Some(rest) = v.strip_prefix('"') {
        let mut out = String::new();
        let mut chars = rest.chars();
        while let Some(c) = chars.next() {
            match c {
                '"' => return (!out.is_empty()).then_some(out),
                '\\' => match chars.next()? {
                    'n' => out.push('\n'),
                    't' => out.push('\t'),
                    'r' => out.push('\r'),
                    c @ ('"' | '\\') => out.push(c),
                    // `\u`, `\U`, `\b`, a line-ending backslash: readable, but
                    // not by fifteen lines, and a half-decoded value is worse
                    // than an absent key.
                    _ => return None,
                },
                c => out.push(c),
            }
        }
        // Unterminated: the value continues onto another line, or the manifest
        // is broken. Either way it is not something this read.
        return None;
    }
    if let Some(rest) = v.strip_prefix('\'') {
        // A literal string has no escapes, by definition.
        let (inner, _) = rest.split_once('\'')?;
        return (!inner.is_empty()).then(|| inner.to_string());
    }
    // A bare value: `2021`, `1.75`, `true`, a date. A comment may follow, and
    // a bare TOML value can never contain a `#`.
    let bare = v.split('#').next().unwrap_or("").trim();
    (!bare.is_empty() && !bare.starts_with(['[', '{'])).then(|| bare.to_string())
}

fn node(cwd: &Path, manifest: &Path, s: &mut Survey) {
    let Ok(text) = fs::read_to_string(manifest) else {
        return;
    };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) else {
        return;
    };
    let typescript = cwd.join("tsconfig.json").is_file();
    s.languages.push(if typescript {
        "typescript"
    } else {
        "javascript"
    });
    if let Some(name) = json["name"].as_str() {
        s.name = Some(name.to_string());
    }
    if let Some(desc) = json["description"].as_str() {
        s.purpose = Some(desc.to_string());
    }
    if let Some(main) = json["main"].as_str() {
        s.entry_points.push(Entry {
            path: main.trim_start_matches("./").to_string(),
            kind: "main",
        });
    }
    if let Some(bins) = json["bin"].as_object() {
        for p in bins.values().filter_map(|v| v.as_str()) {
            s.entry_points.push(Entry {
                path: p.trim_start_matches("./").to_string(),
                kind: "binary",
            });
        }
    }
    // Scripts that exist, spelled the way npm runs them. `test` and `start`
    // have their own verbs; everything else goes through `run`. This is the one
    // place `commands` is genuinely read out of a manifest rather than
    // conventional.
    if let Some(scripts) = json["scripts"].as_object() {
        for name in ["build", "test", "start", "dev", "lint"] {
            if !scripts.contains_key(name) {
                continue;
            }
            let spelled = match name {
                "test" | "start" => format!("npm {name}"),
                _ => format!("npm run {name}"),
            };
            let verb = match name {
                "build" => "build",
                "test" => "test",
                "start" | "dev" => "run",
                _ => "lint",
            };
            s.commands.push((verb, spelled));
        }
    }
}

fn python(cwd: &Path, manifest: &Path, s: &mut Survey) {
    let Ok(text) = fs::read_to_string(manifest) else {
        return;
    };
    s.languages.push("python");
    if let Some(name) = toml_string(&text, "[project]", "name") {
        s.name = Some(name);
    }
    if let Some(desc) = toml_string(&text, "[project]", "description") {
        s.purpose = Some(desc);
    }
    if let Some(v) = toml_string(&text, "[project]", "requires-python") {
        s.toolchain.insert("python", vec![("requires", v)]);
    }
    for candidate in ["main.py", "__main__.py", "app.py"] {
        if cwd.join(candidate).is_file() {
            s.entry_points.push(Entry {
                path: candidate.into(),
                kind: "main",
            });
        }
    }
    // Only a test runner the project itself configured. Nothing here assumes
    // pytest is installed because a `tests/` directory exists.
    if text.contains("[tool.pytest") {
        s.commands.push(("test", "pytest".into()));
    }
}

fn go(cwd: &Path, s: &mut Survey) {
    s.languages.push("go");
    if let Ok(text) = fs::read_to_string(cwd.join("go.mod")) {
        if let Some(module) = text
            .lines()
            .find_map(|l| l.trim().strip_prefix("module ").map(str::trim))
        {
            if let Some(last) = module.rsplit('/').next().filter(|l| !l.is_empty()) {
                s.name = Some(last.to_string());
            }
        }
    }
    if cwd.join("main.go").is_file() {
        s.entry_points.push(Entry {
            path: "main.go".into(),
            kind: "main",
        });
    }
    s.commands.push(("build", "go build ./...".into()));
    s.commands.push(("test", "go test ./...".into()));
}

// endregion: the survey

// region: rendering
// ---------------------------------------------------------------------------
// The file
//
// Written by hand rather than serialised, because the comments are half of what
// this file is: the contract that says it must be kept current cannot live in a
// struct, and a serialiser would drop it on the first refresh.
// ---------------------------------------------------------------------------

/// A YAML scalar that cannot be misread: quoted, with quotes, backslashes and
/// every control character escaped.
///
/// The quoting is cheap insurance against a README line that starts with a `-`
/// or contains a `:`. The control-character escaping is not decoration: a
/// `package.json` description is arbitrary text, and this repository's rule is
/// that a stream somebody redirects carries no escape bytes. A file is the same
/// class of thing — an ESC or a bare CR copied out of a manifest and into a
/// context file is a byte that rewrites the terminal of whoever `cat`s it.
fn scalar(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Whether the memory wiki this file points at is actually on disk. `write`
/// asks at write time rather than at survey time, so the answer cannot be
/// stale by the width of an `init`.
fn wiki_present(cwd: &Path) -> bool {
    cwd.join(MEMORY_INDEX).is_file()
}

fn mechanical(s: &Survey, today: &str, wiki: bool) -> String {
    let mut y = String::new();
    y.push_str(
        "# .platform/context.yaml: this repository, small enough to read first.\n\
         #\n\
         # THIS FILE IS THE SOURCE OF TRUTH FOR THE REPOSITORY'S SHAPE.\n\
         # Read it before searching. It exists so that a model does not have to\n\
         # spend its context re-deriving what the repository is every time.\n\
         #\n\
         # The contract, and it binds whoever changes this repository:\n\
         #   * If you change the repository's shape (a new top-level directory,\n\
         #     a new entry point, a build or test command that changed, a new\n\
         #     language), you update this file in the same change.\n\
         #   * A stale map is worse than no map. The reader that trusts this file\n\
         #     does not go and check it, and a small model has nothing left over\n\
         #     to notice with.\n\
         #   * The derived zone below is regenerated by `emma init`, which costs\n\
         #     no model call and no network. Run it rather than hand-editing it.\n\
         #   * Depth belongs in the memory wiki, not here. This file stays short\n\
         #     enough to read whole; `see:` entries point at the pages that go\n\
         #     further.\n\
         #   * This file is plaintext and it is committed. No credentials in it,\n\
         #     and nothing about one contributor's machine: it describes the\n\
         #     repository, which is the only thing every reader shares.\n\
         #\n\
         # --- maintained: mechanical. Regenerated by `emma init`; edits here are lost ---\n\n",
    );
    y.push_str(&format!("updated: {today}\n"));
    y.push_str(&format!(
        "staleness: {}\n\n",
        scalar(
            "This file is the source of truth for the shape of this repository. \
             Update it in the same change that changes the repository. If `updated:` \
             is older than the last change to the layout, entry points or commands \
             below, treat those sections as unverified and re-run `emma init`."
        )
    ));

    y.push_str("repo:\n");
    match &s.name {
        // A manifest named it, or failing that the checkout's own directory —
        // which is one contributor's spelling and can differ from another's.
        Some(n) => y.push_str(&format!("  name: {}\n", scalar(n))),
        None => y.push_str(
            "  name: null    # no manifest named it and the path has no last component\n",
        ),
    }
    match &s.purpose {
        // `null` rather than an empty string or a cheerful placeholder: nobody
        // said what this repository is for, and the file says exactly that.
        None => y.push_str("  purpose: null    # no README line and no manifest description\n"),
        Some(p) => y.push_str(&format!("  purpose: {}\n", scalar(p))),
    }

    y.push_str(
        "\n# One entry per manifest found at the top level. Not a scan of file\n\
         # extensions: a Rust file in a repository with no Cargo.toml is not here.\n\
         languages: [",
    );
    y.push_str(&s.languages.join(", "));
    y.push_str("]\n");

    y.push_str("\n# Keys read verbatim out of a manifest. A key that could not be read is\n# absent rather than guessed.\ntoolchain:");
    if s.toolchain.is_empty() {
        y.push_str(" {}\n");
    } else {
        y.push('\n');
        for (lang, keys) in &s.toolchain {
            y.push_str(&format!("  {lang}:\n"));
            for (k, v) in keys {
                y.push_str(&format!("    {k}: {}\n", scalar(v)));
            }
        }
    }

    y.push_str("\n# Where execution starts. Every one of these is a file seen on disk.\n# Open them before anything else.\nentry_points:");
    if s.entry_points.is_empty() {
        y.push_str(" []\n");
    } else {
        y.push('\n');
        for e in &s.entry_points {
            y.push_str(&format!(
                "  - path: {}\n    kind: {}\n",
                scalar(&e.path),
                e.kind
            ));
        }
    }

    y.push_str(
        "\n# Top-level directories that exist. `role` is read off the directory's\n\
         # NAME against a fixed table of conventions -- nothing looked inside, so a\n\
         # `docs/` full of code still reads as documentation. The two exceptions,\n\
         # `cargo crate` and `cargo workspace members`, are a Cargo.toml seen on\n\
         # disk. A `role: null` is a gap for a human to fill in the curated zone\n\
         # below; it is not a directory nobody uses. Build output, dot-directories\n\
         # and plain names in the repository's own .gitignore are left out: a\n\
         # directory git does not track is not part of the repository's shape.\nlayout:",
    );
    if s.layout.is_empty() {
        y.push_str(" []\n");
    } else {
        y.push('\n');
        for d in &s.layout {
            y.push_str(&format!("  - path: {}\n", scalar(&d.path)));
            match &d.role {
                Some(r) => y.push_str(&format!("    role: {}\n", scalar(r))),
                None => y.push_str("    role: null\n"),
            }
        }
    }

    y.push_str(
        "\n# How this repository is conventionally built and tested. These are the\n\
         # language's standard invocations, emitted because a manifest for that\n\
         # language is present: `cargo build` was NOT read out of Cargo.toml, and\n\
         # nothing here ran them or checked the tool is installed. The npm lines\n\
         # are the exception -- those script names were read from package.json.\n\
         # One command per verb; where two manifests offer the same verb, the\n\
         # first detected wins (rust, then node, then python, then go).\ncommands:",
    );
    if s.commands.is_empty() {
        y.push_str(" {}\n");
    } else {
        y.push('\n');
        let mut seen: Vec<&str> = Vec::new();
        for (k, v) in &s.commands {
            if seen.contains(k) {
                continue;
            }
            seen.push(k);
            y.push_str(&format!("  {k}: {}\n", scalar(v)));
        }
    }

    // Pointed at only when it is there. A path to a file that does not exist
    // is the same lie as a stale map, in one line.
    if wiki {
        y.push_str(&format!(
            "\n# The long version. This file is the summary; the wiki is the depth, and\n\
             # `schema.md` is the discipline for maintaining it.\n\
             memory:\n  index: {}\n  schema: {}\n",
            scalar(MEMORY_INDEX),
            scalar(MEMORY_SCHEMA)
        ));
    } else {
        y.push_str(
            "\n# No memory wiki on disk when this was written, so there is nothing to\n\
             # point at. `emma init` creates one; re-run it and this becomes a pair of\n\
             # paths.\nmemory: {}\n",
        );
    }

    y.push_str("\n# --- end: mechanical ---\n");
    y
}

/// The curated zone as it is born: the slots, empty, and the instructions for
/// filling them. Written once and then never again.
fn curated(s: &Survey) -> String {
    let mut y = String::from(CURATED_FENCE);
    y.push_str(
        "\n#\n\
         # Everything below is yours. `emma init` reads past it and rewrites nothing\n\
         # here, so this is where knowledge that cannot be derived from a manifest\n\
         # goes: what this repository is actually for, which directory holds the\n\
         # decision you keep having to re-find, what not to touch.\n\
         #\n\
         # Not here: passwords, tokens, API keys, anything you would not put in a\n\
         # commit. This file is plaintext and it goes in the repository.\n\
         #\n\
         # `see:` is the progressive-disclosure slot. A wiki page is a quoted\n\
         # string, because bare [[slug]] is a nested list in YAML and not a link:\n\
         #\n\
         #   areas:\n\
         #     \"src\": { see: [\"[[the-parser]]\", \"[[why-two-passes]]\"] }\n\
         #\n\
         # The pages live in .emma/memory/pages/. Start from .emma/memory/index.md.\n\n",
    );
    y.push_str("summary: null\n\n");
    y.push_str("# One entry per area worth going deeper on. Keyed by the paths above.\nareas:\n");
    if s.layout.is_empty() {
        y.push_str("  {}\n");
    } else {
        for d in &s.layout {
            // Quoted, like every other value here. A directory may legally be
            // called `y`, `no`, `on` or `12:30`, and each of those is a YAML
            // scalar that is not the string it looks like.
            y.push_str(&format!("  {}:\n    see: []\n", scalar(&d.path)));
        }
    }
    y.push_str("\n# Things a newcomer gets wrong here. One line each.\ngotchas: []\n");
    y
}

/// Which of the two things [`write`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wrote {
    Created,
    Refreshed,
}

/// Write or refresh `.platform/context.yaml`, and say which of the two it was.
///
/// A refresh replaces the derived zone and keeps the curated one byte for
/// byte. Two files are **refused** rather than rewritten, because both are
/// cases where overwriting costs work nobody can get back:
///
/// - one with no curated fence in it — either not this file, or somebody
///   deleted the seam, and a refresh cannot tell what was derived from what
///   was written;
/// - one that exists and cannot be read — not UTF-8, no permission, a
///   directory. The branch this came from treated every read error as
///   "absent" and created a fresh file over the top. A context file saved
///   once as UTF-16 would have taken its curated zone with it.
///
/// The bytes land through a sibling temporary file and a rename, so an
/// interrupted write leaves the old file whole rather than half of it.
pub fn write(cwd: &Path, s: &Survey) -> Result<Wrote> {
    let file = path(cwd);
    let fresh = mechanical(s, &clock::today(), wiki_present(cwd));
    let (text, wrote) = match fs::read_to_string(&file) {
        Ok(existing) => {
            let Some(at) = existing.find(CURATED_FENCE) else {
                bail!(
                    "{} exists but has no curated fence in it, so a refresh cannot tell what \
                     was derived from what somebody wrote. Move it aside and run `init` again. \
                     The line it looks for is:\n{CURATED_FENCE}",
                    file.display()
                );
            };
            (format!("{fresh}\n{}", &existing[at..]), Wrote::Refreshed)
        }
        Err(e) if e.kind() == ErrorKind::NotFound => {
            (format!("{fresh}\n{}", curated(s)), Wrote::Created)
        }
        Err(e) => bail!(
            "{} exists but could not be read ({e}), so a refresh cannot tell what somebody \
             wrote in it. Nothing was written. If it is text in another encoding, save it as \
             UTF-8; otherwise move it aside and run `init` again.",
            file.display()
        ),
    };
    let dir = file.parent().expect("context.yaml always has a parent");
    fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    replace(&file, &text).with_context(|| format!("writing {}", file.display()))?;
    Ok(wrote)
}

/// Put `text` at `file` without ever leaving `file` half-written.
///
/// `fs::write` truncates first and then copies, so a process that dies in
/// between leaves a shortened file — and the tail of this one is a person's
/// prose, which the derived half above it cannot reconstruct. A temporary
/// sibling and a rename cost one extra file and remove that window on both
/// platforms this runs on: `fs::rename` is `MoveFileEx` with
/// `REPLACE_EXISTING` on Windows and `rename(2)` on unix, and both replace an
/// existing destination in one step.
///
/// The temporary is removed if the rename fails, so a failed refresh does not
/// leave litter next to the file it could not replace.
fn replace(file: &Path, text: &str) -> Result<()> {
    let tmp = file.with_extension("yaml.tmp");
    fs::write(&tmp, text).with_context(|| format!("writing {}", tmp.display()))?;
    if let Err(e) = fs::rename(&tmp, file) {
        let _ = fs::remove_file(&tmp);
        return Err(e).with_context(|| format!("replacing {}", file.display()));
    }
    Ok(())
}

// endregion: rendering

// region: the orientation file
// ---------------------------------------------------------------------------
// CLAUDE.md / AGENTS.md
//
// The file every other agent already reads. Emma does not need it (it reads
// `.emma/`), so what goes in it is one pointer at the fast path and one at the
// wiki, and nothing else. A repository that already has one gets the pointer
// appended and keeps every word that was in it.
// ---------------------------------------------------------------------------

/// The marker that makes appending idempotent. A comment, so it is invisible
/// wherever the file is rendered.
const MARKER: &str = "<!-- emma:init -->";

const POINTER: &str = "\
<!-- emma:init -->
## Orientation

Read `.platform/context.yaml` first. It is the repository's shape: languages,
entry points, directory map, build and test commands, kept short enough to
read whole, and it is the source of truth for those facts. If you change the
shape of this repository, update that file in the same change.

Depth lives in the memory wiki at `.emma/memory/`: start at `index.md`, which
is the catalog, and open only the pages it points at. `schema.md` there says how
pages are written and maintained.
";

/// Add the pointer to whichever orientation files this repository has, or write
/// a minimal `AGENTS.md` if it has none. Returns every file touched.
///
/// `AGENTS.md` and not `CLAUDE.md` for the file it creates: the pointer is
/// about this repository rather than about one vendor's agent, and a
/// repository that later wants `CLAUDE.md` still gets the pointer appended to
/// it.
///
/// **Every** file, not the last one — the branch returned a single `Option`
/// assigned inside the loop, so a repository with both `CLAUDE.md` and
/// `AGENTS.md` reported one of the two files it had just edited.
pub fn orient(cwd: &Path) -> Result<Vec<PathBuf>> {
    let existing: Vec<PathBuf> = ["CLAUDE.md", "AGENTS.md"]
        .iter()
        .map(|n| cwd.join(n))
        .filter(|p| p.is_file())
        .collect();
    if existing.is_empty() {
        let path = cwd.join("AGENTS.md");
        let text = format!("# Agent orientation\n\n{POINTER}");
        fs::write(&path, text).with_context(|| format!("writing {}", path.display()))?;
        return Ok(vec![path]);
    }
    let mut touched = Vec::new();
    for path in existing {
        let text =
            fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        if text.contains(MARKER) {
            continue;
        }
        let joiner = if text.ends_with('\n') { "\n" } else { "\n\n" };
        fs::write(&path, format!("{text}{joiner}{POINTER}"))
            .with_context(|| format!("appending to {}", path.display()))?;
        touched.push(path);
    }
    Ok(touched)
}

// endregion: the orientation file

// region: the starter page
// ---------------------------------------------------------------------------
// One wiki page, and only what a manifest already said
//
// The temptation here is to write the model's first three pages for it. Every
// sentence of those would be invented, and an invented page in a wiki whose
// whole value is that its claims were checked is a worse start than an empty
// one. So: the repository, its languages, its entry points, and a pointer at
// the file that has the rest. All of it copied, none of it inferred.
// ---------------------------------------------------------------------------

/// Create the repository's own page if the wiki has not got one.
///
/// Returns the slug when it wrote one. Called on every `init`, and writes on
/// the first only: a second page about the same repository is exactly the
/// duplication `schema.md` forbids.
///
/// A survey with no name writes nothing at all. There is no title to give the
/// page, and `slugify("")` is the string `memory` — a page called that,
/// filed under Projects, describing a repository nobody could name.
pub fn starter_page(wiki: &crate::memory::Wiki, s: &Survey) -> Result<Option<String>> {
    let Some(name) = s.name.as_deref() else {
        return Ok(None);
    };
    let slug = crate::memory::slugify(name);
    if wiki.read(&slug).is_ok() {
        return Ok(None);
    }
    let mut body = String::new();
    body.push_str(&format!(
        "`{}`{}\n\n",
        name,
        match &s.purpose {
            Some(p) => format!(": {p}"),
            None => String::new(),
        }
    ));
    body.push_str(&format!(
        "- Languages: {}\n",
        if s.languages.is_empty() {
            "none detected from a manifest".to_string()
        } else {
            s.languages.join(", ")
        }
    ));
    if !s.entry_points.is_empty() {
        body.push_str(&format!(
            "- Entry points: {}\n",
            s.entry_points
                .iter()
                .map(|e| format!("`{}`", e.path))
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    body.push_str(&format!(
        "\nThe repository's shape (directory map, commands, toolchain) is in \
         `{DIR}/{FILE}`, which is the source of truth for those facts and is \
         regenerated by `emma init`. This page is for what that file cannot \
         derive: decisions, conventions, and why things are the way they are.\n\n\
         Everything on this page came from a manifest and a README. Nothing here \
         was inferred.\n"
    ));
    wiki.create(name, crate::memory::Category::Projects, "emma init", &body)?;
    Ok(Some(slug))
}

// endregion: the starter page

#[cfg(test)]
mod tests {
    use super::*;

    /// The curated half of a written file: from the fence to the end, exactly
    /// as it is on disk. Every preservation assertion below compares these
    /// bytes, because "the human's text is still in there somewhere" is a
    /// weaker claim than the one the module makes.
    fn curated_half(text: &str) -> &str {
        let at = text.find(CURATED_FENCE).expect("no fence in the file");
        &text[at..]
    }

    fn read(p: &Path) -> String {
        fs::read_to_string(p).unwrap()
    }

    // -- the manifest reader ------------------------------------------------

    #[test]
    fn a_manifest_key_it_cannot_read_is_left_out_rather_than_guessed() {
        let text = "[package]\nname = \"widget\"\nversion.workspace = true\n";
        assert_eq!(
            toml_string(text, "[package]", "name").as_deref(),
            Some("widget")
        );
        assert_eq!(toml_string(text, "[package]", "edition"), None);
        // A key in another table is not this table's key.
        let two = "[package]\nname = \"a\"\n\n[dependencies]\nedition = \"9\"\n";
        assert_eq!(toml_string(two, "[package]", "edition"), None);
    }

    #[test]
    fn a_value_with_a_comment_after_it_is_the_value_and_not_the_comment() {
        // The branch's `trim_matches('"')` answered `2021" # bump me` here and
        // wrote it into the file as a value that had been "read".
        let quoted = "[package]\nedition = \"2021\"   # bump me\n";
        assert_eq!(
            toml_string(quoted, "[package]", "edition").as_deref(),
            Some("2021")
        );
        let bare = "[package]\nrust-version = 1.75  # MSRV\n";
        assert_eq!(
            toml_string(bare, "[package]", "rust-version").as_deref(),
            Some("1.75")
        );
        let literal = "[package]\nedition = '2021'  # single quotes are TOML too\n";
        assert_eq!(
            toml_string(literal, "[package]", "edition").as_deref(),
            Some("2021")
        );
    }

    #[test]
    fn a_value_this_cannot_finish_reading_is_absent_rather_than_half_decoded() {
        // Unterminated, an escape it does not implement, an inline table, an
        // array. Each of these has a right answer this parser does not know,
        // and a wrong one it must not publish.
        assert_eq!(toml_scalar("\"unterminated"), None);
        assert_eq!(toml_scalar("\"a \\u00e9 b\""), None);
        assert_eq!(toml_scalar("{ workspace = true }"), None);
        assert_eq!(toml_scalar("[\"a\", \"b\"]"), None);
        assert_eq!(toml_scalar("\"\""), None);
        // The escapes it does implement round-trip rather than dropping the
        // backslash and keeping the letter.
        assert_eq!(toml_scalar("\"a\\nb\"").as_deref(), Some("a\nb"));
        assert_eq!(
            toml_scalar("\"say \\\"hi\\\"\"").as_deref(),
            Some("say \"hi\"")
        );
    }

    // -- the README ---------------------------------------------------------

    #[test]
    fn the_readme_line_skips_the_title_and_the_badges() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("README.md"),
            "# Widget\n\n![badge](x)\n<p>html</p>\n\nIt widgets the things.\n",
        )
        .unwrap();
        assert_eq!(
            readme_purpose(dir.path()).as_deref(),
            Some("It widgets the things.")
        );
    }

    #[test]
    fn a_hard_wrapped_readme_gives_a_whole_sentence_and_not_a_line() {
        // The defect this was written from: the first *line* of Emma's own
        // README ends "…it reads its", which reads as a truncation because it
        // is one.
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("README.md"),
            "# Emma\n\nA small agent you run from a terminal. You type `emma`, it reads its\n\
             configuration from the directory you are standing in.\n\nA second paragraph.\n",
        )
        .unwrap();
        assert_eq!(
            readme_purpose(dir.path()).as_deref(),
            Some(
                "A small agent you run from a terminal. You type `emma`, it reads its \
                 configuration from the directory you are standing in."
            )
        );
    }

    #[test]
    fn a_long_paragraph_is_cut_at_a_sentence_and_never_mid_word() {
        let long = format!("{} Second sentence here.", "Words ".repeat(60));
        let cut = first_sentences(&long, 240);
        assert!(cut.chars().count() <= 241, "{cut}");
        assert!(cut.ends_with('…'), "{cut}");
        let two = "One. ".to_string() + &"x".repeat(300);
        assert_eq!(first_sentences(&two, 240), "One.");
    }

    // -- rendering ----------------------------------------------------------

    #[test]
    fn a_purpose_that_would_break_the_yaml_is_quoted() {
        // A README first line beginning with a `-`, or containing a colon, is
        // ordinary prose and a YAML document all by itself.
        let s = Survey {
            name: Some("w".into()),
            purpose: Some("- notes: \"a\" thing".into()),
            ..Survey::default()
        };
        let text = mechanical(&s, "2026-08-26", false);
        assert!(
            text.contains("  purpose: \"- notes: \\\"a\\\" thing\"\n"),
            "{text}"
        );
    }

    #[test]
    fn a_control_byte_out_of_a_manifest_never_reaches_the_file_raw() {
        // A `package.json` description is arbitrary text. An ESC or a bare CR
        // copied into this file rewrites the terminal of whoever reads it, and
        // a raw newline breaks the document besides.
        let s = Survey {
            name: Some("w".into()),
            purpose: Some("red \u{1b}[31m and\r\na newline\tand a tab".into()),
            ..Survey::default()
        };
        let text = mechanical(&s, "2026-08-26", false);
        assert!(
            !text.contains('\u{1b}') && !text.contains('\r'),
            "escape or CR survived: {text:?}"
        );
        assert!(text.contains("\\u001b[31m"), "{text}");
        assert!(text.contains("\\r\\na newline\\tand a tab"), "{text}");
        // One `purpose:` line, still: the raw newline did not split the value.
        assert_eq!(
            text.lines()
                .filter(|l| l.trim_start().starts_with("purpose:"))
                .count(),
            1,
            "{text}"
        );
    }

    #[test]
    fn a_directory_whose_name_is_a_yaml_keyword_is_quoted_as_an_area_key() {
        // `y`, `no` and `on` are booleans in YAML 1.1, and `12:30` is a
        // sexagesimal integer. All four are legal directory names.
        let s = Survey {
            layout: ["y", "no", "on", "12:30"]
                .iter()
                .map(|p| Dir {
                    path: (*p).to_string(),
                    role: None,
                })
                .collect(),
            ..Survey::default()
        };
        let text = curated(&s);
        for name in ["y", "no", "on", "12:30"] {
            assert!(
                text.contains(&format!("  \"{name}\":\n    see: []\n")),
                "{text}"
            );
        }
    }

    #[test]
    fn a_repository_with_no_manifest_claims_nothing_rather_than_something_empty() {
        let dir = tempfile::tempdir().unwrap();
        let s = survey(dir.path());
        assert!(s.languages.is_empty());
        assert!(s.commands.is_empty());
        assert!(s.purpose.is_none());
        let text = mechanical(&s, "2026-08-26", false);
        assert!(text.contains("languages: []\n"), "{text}");
        assert!(text.contains("toolchain: {}\n"), "{text}");
        assert!(text.contains("commands: {}\n"), "{text}");
        assert!(text.contains("entry_points: []\n"), "{text}");
        assert!(text.contains("  purpose: null"), "{text}");
    }

    #[test]
    fn a_path_with_no_last_component_has_no_name_rather_than_a_stand_in() {
        // `name: "repository"` is a value nobody supplied, in the slot a
        // reader trusts most.
        assert_eq!(dir_name(Path::new("/")), None);
        let s = Survey::default();
        let text = mechanical(&s, "2026-08-26", false);
        assert!(text.contains("  name: null"), "{text}");
        assert!(!text.contains("repository\""), "{text}");
    }

    #[test]
    fn the_memory_pointer_is_written_only_when_the_wiki_is_on_disk() {
        let s = Survey::default();
        let without = mechanical(&s, "2026-08-26", false);
        assert!(without.contains("memory: {}"), "{without}");
        assert!(!without.contains(MEMORY_INDEX), "{without}");
        let with = mechanical(&s, "2026-08-26", true);
        assert!(
            with.contains(MEMORY_INDEX) && with.contains(MEMORY_SCHEMA),
            "{with}"
        );
    }

    #[test]
    fn write_asks_the_disk_about_the_wiki_rather_than_assuming_it() {
        // Only the derived half is examined: the curated zone's own prose
        // names the wiki's index as a place to start reading, which is not the
        // machine-readable `memory:` pointer this is about.
        let dir = tempfile::tempdir().unwrap();
        let derived = |t: &str| t.split(CURATED_FENCE).next().unwrap().to_string();

        write(dir.path(), &survey(dir.path())).unwrap();
        let before = derived(&read(&path(dir.path())));
        assert!(before.contains("memory: {}"), "{before}");
        assert!(!before.contains(MEMORY_INDEX), "{before}");

        fs::create_dir_all(dir.path().join(".emma/memory")).unwrap();
        fs::write(dir.path().join(MEMORY_INDEX), "# index\n").unwrap();
        write(dir.path(), &survey(dir.path())).unwrap();
        let after = derived(&read(&path(dir.path())));
        assert!(after.contains(MEMORY_INDEX), "{after}");
    }

    // -- the survey against a real tree -------------------------------------

    /// A workspace shaped like this repository's: no root binary, members two
    /// levels down, and a package name that is not its directory name.
    fn workspace(root: &Path) {
        fs::create_dir_all(root.join("crates/emma/src")).unwrap();
        fs::create_dir_all(root.join("tools/fs/src")).unwrap();
        fs::create_dir_all(root.join("target/debug")).unwrap();
        fs::create_dir_all(root.join(".hidden")).unwrap();
        fs::create_dir_all(root.join("docs")).unwrap();
        fs::write(
            root.join("Cargo.toml"),
            "[workspace.package]\nedition = \"2021\"\n",
        )
        .unwrap();
        fs::write(
            root.join("crates/emma/Cargo.toml"),
            "[package]\nname = \"emma\"\n",
        )
        .unwrap();
        fs::write(root.join("crates/emma/src/main.rs"), "fn main() {}").unwrap();
        fs::write(root.join("crates/emma/src/lib.rs"), "").unwrap();
        fs::write(
            root.join("tools/fs/Cargo.toml"),
            "[package]\nname = \"emma-tools-fs\"\n",
        )
        .unwrap();
        fs::write(root.join("tools/fs/src/lib.rs"), "").unwrap();
    }

    #[test]
    fn a_workspace_run_command_names_the_package_and_not_the_directory() {
        let dir = tempfile::tempdir().unwrap();
        workspace(dir.path());
        let s = survey(dir.path());
        let run = s
            .commands
            .iter()
            .find(|(k, _)| *k == "run")
            .map(|(_, v)| v.as_str());
        assert_eq!(run, Some("cargo run -p emma"));
        assert_eq!(
            s.toolchain.get("rust").map(Vec::as_slice),
            Some(&[("edition", "2021".to_string())][..])
        );
        let paths: Vec<&str> = s.entry_points.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(
            paths,
            [
                "crates/emma/src/main.rs",
                "crates/emma/src/lib.rs",
                "tools/fs/src/lib.rs"
            ]
        );
    }

    #[test]
    fn a_second_binary_means_no_run_line_rather_than_one_that_does_not_run() {
        let dir = tempfile::tempdir().unwrap();
        workspace(dir.path());
        fs::write(dir.path().join("tools/fs/src/main.rs"), "fn main() {}").unwrap();
        let s = survey(dir.path());
        assert!(
            !s.commands.iter().any(|(k, _)| *k == "run"),
            "{:?}",
            s.commands
        );
    }

    #[test]
    fn build_output_and_hidden_directories_are_not_in_the_layout() {
        let dir = tempfile::tempdir().unwrap();
        workspace(dir.path());
        let s = survey(dir.path());
        let names: Vec<&str> = s.layout.iter().map(|d| d.path.as_str()).collect();
        assert_eq!(names, ["crates", "docs", "tools"], "{names:?}");
        let role = |n: &str| {
            s.layout
                .iter()
                .find(|d| d.path == n)
                .and_then(|d| d.role.clone())
        };
        assert_eq!(role("docs").as_deref(), Some("documentation"));
        assert_eq!(role("crates").as_deref(), Some("cargo workspace members"));
    }

    #[test]
    fn the_layout_is_put_in_name_order_and_not_taken_on_trust_from_the_filesystem() {
        // Handed over backwards on purpose. The integration test below cannot
        // prove this on Windows, where `read_dir` is already sorted and the
        // sort could be deleted without a test noticing.
        let names = ["tools", "docs", "crates", "Assets"]
            .map(String::from)
            .to_vec();
        let out: Vec<String> = ordered(names).into_iter().map(|d| d.path).collect();
        assert_eq!(out, ["Assets", "crates", "docs", "tools"]);
    }

    #[test]
    fn a_directory_the_repository_gitignores_is_not_part_of_its_shape() {
        // Certifying against Emma's own tree, not a fixture, is what found
        // this: `memory/` is hook scratch that `.gitignore` excludes, and it
        // was in the map beside `crates` and `docs`.
        let dir = tempfile::tempdir().unwrap();
        workspace(dir.path());
        for d in ["scratch", "keepme", "logs"] {
            fs::create_dir_all(dir.path().join(d)).unwrap();
        }
        fs::write(
            dir.path().join(".gitignore"),
            "# a comment\n/scratch\nlogs/\n!keepme\n*.png\n.emma/tasks/\n\n",
        )
        .unwrap();
        let names: Vec<String> = survey(dir.path())
            .layout
            .into_iter()
            .map(|d| d.path)
            .collect();
        let has = |n: &str| names.iter().any(|p| p == n);
        assert!(!has("scratch"), "{names:?}");
        assert!(!has("logs"), "{names:?}");
        // A negation and a wildcard are not names this drops, and a real
        // directory must never disappear because a pattern was misread.
        assert!(has("keepme"), "{names:?}");
        assert!(has("crates") && has("docs"), "{names:?}");
    }

    #[test]
    fn a_gitignore_comment_never_deletes_a_directory_that_shares_its_text() {
        // `#` and `!` are legal in a directory name on both platforms this
        // runs on, so the comment and negation guards are the difference
        // between a comment and a rule -- without them, writing `#notes` at
        // the top of a .gitignore silently removes `#notes/` from the map.
        let dir = tempfile::tempdir().unwrap();
        for d in ["#notes", "!keepme"] {
            fs::create_dir_all(dir.path().join(d)).unwrap();
        }
        fs::write(dir.path().join(".gitignore"), "#notes\n!keepme\n").unwrap();
        let names: Vec<String> = survey(dir.path())
            .layout
            .into_iter()
            .map(|d| d.path)
            .collect();
        assert!(names.iter().any(|p| p == "#notes"), "{names:?}");
        assert!(names.iter().any(|p| p == "!keepme"), "{names:?}");
    }

    #[test]
    fn two_runs_over_one_unchanged_tree_produce_one_file() {
        // `read_dir` hands entries back in the filesystem's order, which is not
        // the same order on two machines. Without the sort, a context file
        // committed on one box shows a diff on the next.
        let dir = tempfile::tempdir().unwrap();
        workspace(dir.path());
        write(dir.path(), &survey(dir.path())).unwrap();
        let first = read(&path(dir.path()));
        write(dir.path(), &survey(dir.path())).unwrap();
        assert_eq!(first, read(&path(dir.path())));
    }

    // -- the curated fence, which is the whole point ------------------------

    /// What a person writes below the fence: a colon, a quote, CRLF, a tab,
    /// non-ASCII, and no trailing newline. Every byte of this must come back.
    const HUMAN: &str = "\nsummary: \"the parser is in src/, not tools/\"\r\n\
                         areas:\r\n  \"src\":\r\n    see: [\"[[why-two-passes]]\"]\r\n\
                         gotchas:\r\n  - \"naive: do not touch build.rs\"\t\r\n  - final line, no newline";

    /// Append a person's prose to the curated zone of an already-written file.
    fn with_human(cwd: &Path) -> String {
        let file = path(cwd);
        let text = format!("{}{HUMAN}", read(&file));
        assert!(text.contains(CURATED_FENCE), "fixture has no fence");
        fs::write(&file, &text).unwrap();
        text
    }

    #[test]
    fn a_curated_zone_survives_a_refresh_byte_for_byte_while_the_derived_half_moves() {
        let dir = tempfile::tempdir().unwrap();
        workspace(dir.path());
        assert_eq!(
            write(dir.path(), &survey(dir.path())).unwrap(),
            Wrote::Created
        );
        let before = with_human(dir.path());
        let kept = curated_half(&before).to_string();

        // The repository changes shape underneath, so a refresh has something
        // to say: without that this test would pass over a no-op.
        fs::create_dir_all(dir.path().join("benches")).unwrap();
        assert_eq!(
            write(dir.path(), &survey(dir.path())).unwrap(),
            Wrote::Refreshed
        );

        let after = read(&path(dir.path()));
        assert_eq!(
            curated_half(&after),
            kept,
            "the curated zone was not preserved"
        );
        assert!(
            after.contains("benches"),
            "the derived zone did not refresh"
        );
        assert!(!before.contains("benches"), "the fixture was already stale");
        // Exactly one fence: a refresh must not append a second curated zone.
        assert_eq!(after.matches(CURATED_FENCE).count(), 1, "{after}");
    }

    #[test]
    fn a_file_with_no_fence_is_refused_and_left_exactly_as_it_was() {
        let dir = tempfile::tempdir().unwrap();
        let file = path(dir.path());
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        // Somebody's own YAML, or this file with the seam deleted.
        let theirs = "repo:\n  name: mine\nnotes: everything I know\n";
        fs::write(&file, theirs).unwrap();

        let err = write(dir.path(), &survey(dir.path())).unwrap_err();
        assert!(format!("{err}").contains("curated fence"), "{err}");
        assert_eq!(read(&file), theirs, "the file was rewritten anyway");
        assert!(
            !file.with_extension("yaml.tmp").exists(),
            "litter left behind"
        );
    }

    #[test]
    fn an_empty_file_is_refused_rather_than_treated_as_absent() {
        // A zero-byte file is what a crashed editor leaves. It has no fence,
        // so it is somebody's problem to look at, not something to write over.
        let dir = tempfile::tempdir().unwrap();
        let file = path(dir.path());
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, "").unwrap();
        assert!(write(dir.path(), &survey(dir.path())).is_err());
        assert_eq!(read(&file), "");
    }

    #[test]
    fn a_file_that_is_not_utf8_is_refused_rather_than_overwritten() {
        // The branch treated every read error as "the file is not there" and
        // created a fresh one over the top. A context file saved once as
        // UTF-16 -- which is what PowerShell's `Out-File -Encoding unicode`
        // writes -- took its curated zone with it.
        let dir = tempfile::tempdir().unwrap();
        let file = path(dir.path());
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        let mut utf16: Vec<u8> = vec![0xff, 0xfe];
        for b in "summary: mine\n".bytes() {
            utf16.push(b);
            utf16.push(0);
        }
        fs::write(&file, &utf16).unwrap();

        let err = write(dir.path(), &survey(dir.path())).unwrap_err();
        assert!(format!("{err}").contains("could not be read"), "{err}");
        assert_eq!(fs::read(&file).unwrap(), utf16, "the bytes changed");
    }

    #[test]
    fn a_utf8_bom_does_not_hide_the_fence() {
        // PowerShell's `>`, `Out-File` and `Set-Content -Encoding utf8` all
        // write one, and this repository has been bitten by a parser that
        // matched from byte zero.
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), &survey(dir.path())).unwrap();
        let file = path(dir.path());
        let bom = format!("\u{feff}{}", with_human(dir.path()));
        fs::write(&file, &bom).unwrap();
        let kept = curated_half(&bom).to_string();

        assert_eq!(
            write(dir.path(), &survey(dir.path())).unwrap(),
            Wrote::Refreshed
        );
        assert_eq!(curated_half(&read(&file)), kept);
    }

    #[test]
    fn a_fence_reached_through_crlf_is_found_and_its_line_endings_are_kept() {
        // A file round-tripped through a Windows editor has CRLF on every
        // line, the fence's included.
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), &survey(dir.path())).unwrap();
        let file = path(dir.path());
        let crlf = with_human(dir.path())
            .replace("\r\n", "\n")
            .replace('\n', "\r\n");
        fs::write(&file, &crlf).unwrap();
        let kept = curated_half(&crlf).to_string();
        assert!(kept.contains("\r\n"), "the fixture lost its CRLF");

        write(dir.path(), &survey(dir.path())).unwrap();
        assert_eq!(curated_half(&read(&file)), kept);
    }

    #[test]
    fn a_refresh_does_not_leave_a_temporary_file_beside_the_real_one() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), &survey(dir.path())).unwrap();
        write(dir.path(), &survey(dir.path())).unwrap();
        let names: Vec<String> = fs::read_dir(dir.path().join(DIR))
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, [FILE]);
    }

    // -- the orientation file -----------------------------------------------

    #[test]
    fn orient_writes_agents_md_when_the_repository_has_no_orientation_file() {
        let dir = tempfile::tempdir().unwrap();
        let touched = orient(dir.path()).unwrap();
        assert_eq!(touched, [dir.path().join("AGENTS.md")]);
        assert!(!dir.path().join("CLAUDE.md").exists());
        assert!(read(&dir.path().join("AGENTS.md")).contains(MARKER));
    }

    #[test]
    fn orient_appends_once_and_keeps_every_word_that_was_there() {
        let dir = tempfile::tempdir().unwrap();
        let claude = dir.path().join("CLAUDE.md");
        let agents = dir.path().join("AGENTS.md");
        let theirs = "# House rules\n\nDo not touch build.rs.\n";
        fs::write(&claude, theirs).unwrap();
        fs::write(&agents, theirs).unwrap();

        let first = orient(dir.path()).unwrap();
        assert_eq!(
            first,
            [claude.clone(), agents.clone()],
            "both files, not the last"
        );
        for p in [&claude, &agents] {
            let text = read(p);
            assert!(text.starts_with(theirs), "{text}");
            assert_eq!(text.matches(MARKER).count(), 1, "{text}");
        }

        // Idempotent: a second init appends nothing and reports nothing.
        let before = read(&claude);
        assert!(orient(dir.path()).unwrap().is_empty());
        assert_eq!(read(&claude), before);
    }

    // -- the starter page ---------------------------------------------------

    #[test]
    fn the_starter_page_is_written_once_and_claims_only_what_was_read() {
        let dir = tempfile::tempdir().unwrap();
        workspace(dir.path());
        let s = survey(dir.path());
        let wiki = crate::memory::Wiki::project(dir.path()).unwrap();

        let slug = starter_page(&wiki, &s).unwrap().expect("first init writes");
        let page = wiki.read(&slug).unwrap();
        assert!(
            page.body.contains("crates/emma/src/main.rs"),
            "{}",
            page.body
        );
        assert!(
            page.body.contains("Nothing here was inferred."),
            "{}",
            page.body
        );
        // A second init must not file a duplicate about the same repository.
        assert_eq!(starter_page(&wiki, &s).unwrap(), None);
    }

    #[test]
    fn a_repository_with_no_name_gets_no_starter_page_rather_than_one_called_memory() {
        let dir = tempfile::tempdir().unwrap();
        let wiki = crate::memory::Wiki::project(dir.path()).unwrap();
        let nameless = Survey::default();
        assert_eq!(starter_page(&wiki, &nameless).unwrap(), None);
        assert!(wiki.read("memory").is_err());
    }
}
