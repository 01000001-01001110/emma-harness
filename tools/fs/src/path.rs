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

/// Write `bytes` to `file` so a reader never sees a half-written version.
///
/// **Why this is not `std::fs::write`.** That truncates the target and then
/// streams into it, so a crash, a full disk or a kill between the two leaves
/// the file empty or partial — and the thing being overwritten is somebody's
/// source. `tools/tasks` has done it this way since it was written; the file
/// tools, which write far more often and to files nobody has a copy of, did not.
///
/// The temp file sits beside the target rather than in the system temp
/// directory: a rename across filesystems is a copy, and a copy is not atomic.
/// On failure it is removed, because a stray `.emma-tmp` in somebody's
/// repository is a bug report.
///
/// Windows renames onto an existing path only with `ReplaceFile` semantics,
/// which `std::fs::rename` provides; where it still refuses — a reader holding
/// the target open — the error is returned rather than papered over, and the
/// caller reports it as the write failure it is.
pub fn write_atomically(file: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let Some(name) = file.file_name() else {
        return std::fs::write(file, bytes);
    };

    // **A name nothing else is using.** The first version of this used a fixed
    // `<name>.emma-tmp`, which an adversarial review took apart in two ways and
    // both were real: a user who happens to own `config.toml.emma-tmp` had it
    // truncated and then renamed away by a write to `config.toml` — this
    // function destroying a file while claiming to protect one — and two
    // concurrent writes to one target raced through the same temp, so a caller
    // could be told its content was written when the other call's content
    // landed.
    //
    // Process id and a counter, not randomness: the counter makes two writes in
    // one process distinct, and the pid makes two processes distinct. Both are
    // needed and neither is enough alone.
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut stem = name.to_os_string();
    stem.push(format!(".emma-tmp.{}.{seq}", std::process::id()));
    let tmp = file.with_file_name(stem);

    if let Err(e) = std::fs::write(&tmp, bytes) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    // **Carry the target's permissions across.** The temp file is created fresh,
    // so it takes the process umask — commonly `0644`. Renaming it over a file
    // that was `0600` therefore *widened* it, turning a private file world
    // readable as a side effect of editing it. The same review found that, and
    // it is the sharper of the two: a clobbered temp file is loud eventually,
    // where a permission that quietly widened is not.
    #[cfg(unix)]
    if let Ok(meta) = std::fs::metadata(file) {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(
            &tmp,
            std::fs::Permissions::from_mode(meta.permissions().mode()),
        );
    }
    match std::fs::rename(&tmp, file) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

/// How many names this file has on disk, when that can be told.
///
/// **[`write_atomically`] severs a hard link, and nothing else says so.** The
/// rename replaces the directory entry rather than writing through it, which is
/// the property that makes the write atomic and also the property that breaks
/// the link: the edited name gets the new content, every other name keeps the
/// old, and the link count drops to one on each. Measured on NTFS, not argued —
/// a plain `fs::write` writes through and both names change; temp-and-rename
/// does not. Most editors behave the same way, and a torn file on a crash is
/// worse than a broken link, so this is not an argument for going back. It is a
/// consequence somebody has to be told about, because silent divergence between
/// two names of one file is not something a user will attribute to their editor.
///
/// `None` means *could not be told*, never *one name*: an unreadable file, a
/// filesystem that does not report it, or a platform arm that has no way to ask.
/// A caller must not print "1" for `None` — that is the difference between a
/// measurement and an assumption.
///
/// The two arms ask different questions of the same fact. Unix has `nlink` on
/// the stat every metadata call already did. Windows keeps the count in
/// `BY_HANDLE_FILE_INFORMATION`, which needs an open handle;
/// `MetadataExt::number_of_links` wraps exactly this and is behind the unstable
/// `windows_by_handle` feature, so a stable build has to make the call itself.
#[cfg(unix)]
pub fn hard_links(file: &Path) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    // `symlink_metadata`, not `metadata`: the link count wanted is the one for
    // the entry about to be replaced, and following a symlink would report the
    // target's instead.
    std::fs::symlink_metadata(file).ok().map(|m| m.nlink())
}

#[cfg(windows)]
pub fn hard_links(file: &Path) -> Option<u64> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        GetFileInformationByHandle, BY_HANDLE_FILE_INFORMATION,
    };

    // A plain read handle. Opening for read shares with other readers and does
    // not need write access, so asking this question cannot fail a write that
    // would otherwise have succeeded — the reason it is asked before the write
    // rather than during it.
    let f = std::fs::File::open(file).ok()?;
    let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
    // SAFETY: `f` is open for the duration of the call, and `info` is a
    // correctly-sized, zeroed instance of the struct the call writes into.
    let ok = unsafe { GetFileInformationByHandle(f.as_raw_handle() as _, &mut info) };
    if ok == 0 {
        return None;
    }
    Some(u64::from(info.nNumberOfLinks))
}

#[cfg(not(any(unix, windows)))]
pub fn hard_links(_file: &Path) -> Option<u64> {
    None
}

/// The sentence to append to a write's outcome when it is about to break a
/// link, or nothing.
///
/// Said rather than refused, the same rule the awkward-filename note follows:
/// the write is what was asked for, it succeeds, and refusing it would be Emma
/// deciding how a user may arrange their filesystem. **Read before the write**,
/// because after the rename the count is one and the fact is gone.
pub fn severed_link_note(file: &Path) -> Option<String> {
    match hard_links(file) {
        Some(n) if n > 1 => Some(format!(
            "this file had {n} names on disk (hard links); writing it replaces the \
             directory entry, so the other {} keeps the old content and the link is broken",
            if n == 2 { "name" } else { "names" }
        )),
        _ => None,
    }
}

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
