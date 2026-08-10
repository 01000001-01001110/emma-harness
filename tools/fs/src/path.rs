//! Containment: every path a tool touches resolves inside `ToolCtx::cwd`.
//!
//! The check is not "does the string contain `..`". A symlink inside the root
//! pointing outward is an escape that contains no `..` at all, and a path that
//! does not exist yet cannot be canonicalised at all. So resolution happens in
//! three moves — normalise lexically, canonicalise the longest part that
//! actually exists, re-attach the part that does not — and containment is
//! asserted on the result, which is also the path the caller then uses for I/O.
//! Checking one path and operating on another is the classic way this check
//! becomes decorative.
//!
//! **What this cannot catch, stated so nobody assumes otherwise.** A *hard*
//! link inside the root to a file outside it is not detectable this way: there
//! is no "real" path to canonicalise towards, both names are equally the file,
//! and `canonicalize` returns the inside one. Verified, not assumed — on
//! Windows `mklink /H` needs no privilege at all. Containment here is a
//! boundary against paths, not against an operator who has already placed a
//! link inside the tree, and the same is true of a bind mount. `Bash` is a
//! wider hole than either and is documented as one.
//!
//! Everything this *does* catch was attempted rather than assumed, and the
//! attempts live in `tests/containment.rs`: `..` leading and buried mid-path,
//! an absolute path elsewhere on the machine, a symlinked file and a symlinked
//! directory read through, a file created through a symlinked directory, and
//! `Glob`/`Grep` walking out through a link. That file also carries the
//! positive control — an absolute path *inside* the root still works — because
//! the cheapest wrong way to pass every escape test is to reject all absolute
//! paths, and someone will eventually try it.
//!
//! The file reads top to bottom in the order a call uses it: [`root`] fixes the
//! boundary, [`resolve`] places a path against it, [`resolve_existing`] adds
//! "and it must already be there", the two private helpers do the lexical and
//! canonical halves of the work, and [`display`] turns a result back into
//! something worth showing a model.

use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};

use emma_tool_api::{ToolCtx, ToolError};

// region: Containment
// ---------------------------------------------------------------------------
// Containment
//
// The three entry points every tool goes through. Each returns the path the
// caller then uses for I/O, which is the property that matters: checking one
// path and operating on another is how this check becomes decorative.
// ---------------------------------------------------------------------------

/// The canonical root. Canonicalised once per call so that comparisons below
/// are against the same spelling the OS uses — on Windows that means the
/// `\\?\` verbatim form, and comparing a verbatim path to a non-verbatim one
/// silently never matches, which would fail *open* if the check were inverted.
pub fn root(ctx: &ToolCtx) -> Result<PathBuf, ToolError> {
    ctx.cwd.canonicalize().map_err(|e| {
        ToolError::Unavailable(format!(
            "working directory {} cannot be resolved: {e}",
            ctx.cwd.display()
        ))
    })
}

/// Resolve `raw` against `root`, refusing anything that lands outside it.
///
/// Accepts paths that do not exist yet — `Write` has to be able to name a file
/// it is about to create — by canonicalising the deepest existing ancestor.
///
/// An absolute `raw` is taken as given rather than rejected, and then contained
/// like everything else. That is the deliberate shape: absolute-inside-the-root
/// is a legitimate call the model makes constantly, and the containment
/// assertion is what tells the two apart.
///
/// `starts_with` is `Path`'s, so the comparison is component-wise. A sibling
/// directory whose name merely begins with the root's — `/work` and
/// `/workspace` — does not match, which a string prefix test would have got
/// wrong in the direction that fails open.
pub fn resolve(root: &Path, raw: &str) -> Result<PathBuf, ToolError> {
    if raw.trim().is_empty() {
        return Err(ToolError::BadArguments("path is empty".into()));
    }

    let asked = Path::new(raw);
    let joined = if asked.is_absolute() {
        asked.to_path_buf()
    } else {
        root.join(asked)
    };

    let lexical = lexical_normalize(&joined);
    let (mut out, tail) = canonical_prefix(&lexical);
    for component in tail {
        out.push(component);
    }

    if !out.starts_with(root) {
        return Err(ToolError::BadArguments(format!(
            "{raw} resolves to {}, which is outside the working directory {}",
            out.display(),
            root.display()
        )));
    }
    Ok(out)
}

/// Resolve a path that must already exist, reporting the two failures
/// separately: `BadArguments` when the path is wrong, `Unavailable` when the
/// filesystem will not answer. The model routes differently on each — a wrong
/// path is fixed by naming another, an unreadable mount is not fixed by
/// retrying.
pub fn resolve_existing(root: &Path, raw: &str) -> Result<(PathBuf, std::fs::Metadata), ToolError> {
    let path = resolve(root, raw)?;
    match std::fs::symlink_metadata(&path) {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(ToolError::BadArguments(format!("{raw} does not exist")));
        }
        Err(e) => return Err(ToolError::Failed(format!("{raw} cannot be stat'd: {e}"))),
    }
    // Metadata is taken through the symlink deliberately: `resolve` has already
    // proven the target is inside the root, so following it is safe here and
    // reporting the link's own zero length would be a lie about the file.
    match std::fs::metadata(&path) {
        Ok(meta) => Ok((path, meta)),
        Err(e) => Err(ToolError::Failed(format!("{raw} cannot be stat'd: {e}"))),
    }
}

// endregion: Containment

// region: How a path is resolved, and how it reads back
// ---------------------------------------------------------------------------
// How a path is resolved, and how it reads back
//
// The two halves of the trick that lets a path which does not exist yet still
// be contained — normalise lexically, then canonicalise as much of it as is
// real — and the inverse, turning a resolved path back into something worth
// putting in front of a model.
// ---------------------------------------------------------------------------

/// Drop `.` and resolve `..` textually. Popping past the filesystem root is a
/// no-op, matching POSIX `/..`; the containment assertion — not this function —
/// is what rejects the attempt.
fn lexical_normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    let mut depth = 0usize;
    for c in p.components() {
        match c {
            Component::Prefix(_) | Component::RootDir => out.push(c.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                if depth > 0 {
                    out.pop();
                    depth -= 1;
                }
            }
            Component::Normal(s) => {
                out.push(s);
                depth += 1;
            }
        }
    }
    out
}

/// Split into (canonicalised longest existing ancestor, remaining components).
/// Input must already be lexically normal, so the tail can never contain `..`
/// and re-attaching it cannot walk anywhere the prefix did not permit.
///
/// Searching from the deepest cut downwards is what makes a symlinked parent
/// visible: `escape/planted.txt` cannot be canonicalised, but `escape` can, and
/// it canonicalises to wherever the link points — so the containment check sees
/// the outside path rather than the inside spelling.
///
/// When nothing at all canonicalises the input is returned unchanged with an
/// empty tail. That is not a bypass: an uncanonicalisable path is one whose
/// every ancestor is missing, so there is no link to have been followed, and it
/// still faces the same containment assertion in [`resolve`].
fn canonical_prefix(p: &Path) -> (PathBuf, Vec<OsString>) {
    let comps: Vec<Component> = p.components().collect();
    for cut in (1..=comps.len()).rev() {
        let candidate: PathBuf = comps[..cut].iter().collect();
        if let Ok(canon) = candidate.canonicalize() {
            let tail = comps[cut..]
                .iter()
                .map(|c| c.as_os_str().to_os_string())
                .collect();
            return (canon, tail);
        }
    }
    (p.to_path_buf(), Vec::new())
}

/// How a path reads back to a human: relative to the root when it is inside it,
/// because absolute paths in tool output are noise the model then copies.
///
/// Backslashes become forward slashes so a path reads the same whichever
/// machine produced it, and so a path the model copies into a `Glob` pattern is
/// already in the spelling the matcher expects.
pub fn display(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

// endregion: How a path is resolved, and how it reads back
