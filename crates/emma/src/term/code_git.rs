//! Git backing for the Code page: the repository's files, one file's commit
//! history, and the diff a commit introduced to it.
//!
//! The rule this module shares with [`super::diff`] and [`super::memory`]:
//! **the parsing is pure and tested, the process call is a thin wrapper the
//! tests do not exercise.** Nothing here fabricates — an empty repo, a path
//! with no history, a binary file all resolve to an empty vector, never
//! invented rows.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The unit separator git writes between `--format` fields. A commit subject
/// may contain anything but a newline, so no field delimiter can be a space or
/// a tab; `\x1f` is the byte reserved for exactly this.
const US: char = '\u{1f}';

/// The largest file [`read_file`] will show. Past it the pane says how big the
/// file is instead of drawing the first megabyte, which would be a lie by
/// omission — the [`super::diff`] rule.
pub const MAX_FILE: u64 = 1024 * 1024;

/// The most paths [`paths`] returns, so a monorepo cannot make the tree cost a
/// swap storm on the interactive path.
const MAX_PATHS: usize = 20_000;

/// How a text file joins its lines on disk. Detected from the file's own bytes
/// when read; passed back unchanged on save so a CRLF working tree is not
/// silently normalised to LF.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineEnding {
    Lf,
    CrLf,
}

impl LineEnding {
    fn as_str(self) -> &'static str {
        match self {
            Self::Lf => "\n",
            Self::CrLf => "\r\n",
        }
    }
}

/// One row of a file's history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Commit {
    pub hash: String,
    pub short: String,
    pub author: String,
    pub date: String,
    pub subject: String,
}

/// What a unified-diff line is, which decides its gutter glyph and [`Role`].
/// The order matters at the classify site: `+++`/`---` are `Meta`, and must be
/// tested before the bare `+`/`-` that make an addition or a removal.
///
/// [`Role`]: super::palette::Role
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffKind {
    Add,
    Del,
    Hunk,
    Meta,
    Context,
}

/// One line of a unified diff, tagged. The text is verbatim, gutter and all,
/// so a reader copies exactly what `git diff` would have shown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffRow {
    pub kind: DiffKind,
    pub text: String,
}

/// A readable text file plus the line-ending metadata needed to write it back
/// byte-identically.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextFile {
    pub lines: Vec<String>,
    pub ending: LineEnding,
    /// Whether the file ended with a line terminator (`\n` or `\r\n`).
    pub trailing_newline: bool,
}

/// The result of trying to show a file: its lines, or the honest refusal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileRead {
    Text(TextFile),
    Refused(String),
}

/// What a save attempt did. `ChangedOnDisk` is the case the editor exists to
/// not get wrong: the buffer was opened from one version of the file and
/// something else rewrote it since, so writing would drop the other edit
/// silently. Nothing is written in that arm.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Saved {
    /// Written. The hash is of the bytes just put on disk, and becomes the
    /// buffer's new baseline.
    Ok(u64),
    /// The file on disk is not the one the buffer was opened from.
    ChangedOnDisk,
    /// The write never started, and why.
    Refused(String),
}

/// A content hash, used only to answer "is this the same bytes we opened".
///
/// `DefaultHasher` is not stable across releases of the standard library and
/// is not a checksum anybody should persist. That is fine here and only here:
/// both sides of every comparison are computed by the same running process,
/// within one editing session, and the value is never written down.
pub fn hash_bytes(bytes: &[u8]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut h);
    h.finish()
}

/// The hash of a file's current bytes, or `None` when it cannot be read.
pub fn file_hash(path: &Path) -> Option<u64> {
    std::fs::read(path).ok().map(|b| hash_bytes(&b))
}

// region: Pure parsers
// ---------------------------------------------------------------------------
// Pure parsers — tested; no process, no filesystem
// ---------------------------------------------------------------------------

/// Parse the output of
/// `git log --follow --date=short --format=%H\x1f%h\x1f%an\x1f%ad\x1f%s`.
/// One record per line, five `\x1f`-separated fields; a record missing its
/// hash is dropped rather than rendered half-formed.
pub fn parse_log(stdout: &str) -> Vec<Commit> {
    stdout
        .lines()
        .filter_map(|line| {
            if line.is_empty() {
                return None;
            }
            let mut f = line.split(US);
            let hash = f.next()?.to_string();
            if hash.is_empty() {
                return None;
            }
            Some(Commit {
                hash,
                short: f.next().unwrap_or("").to_string(),
                author: f.next().unwrap_or("").to_string(),
                date: f.next().unwrap_or("").to_string(),
                subject: f.next().unwrap_or("").to_string(),
            })
        })
        .collect()
}

/// Parse a unified diff (the `-p` body of `git show`) into tagged rows.
pub fn parse_diff(stdout: &str) -> Vec<DiffRow> {
    stdout
        .lines()
        .map(|line| DiffRow {
            kind: classify(line),
            text: line.to_string(),
        })
        .collect()
}

/// Which [`DiffKind`] a raw diff line is. `+++`/`---` (the file headers) are
/// tested before the bare `+`/`-`, or every diff's header would read as two
/// spurious edits.
fn classify(line: &str) -> DiffKind {
    if line.starts_with("@@") {
        DiffKind::Hunk
    } else if line.starts_with("+++")
        || line.starts_with("---")
        || line.starts_with("diff ")
        || line.starts_with("index ")
        || line.starts_with("new file")
        || line.starts_with("deleted file")
        || line.starts_with("similarity ")
        || line.starts_with("rename ")
        || line.starts_with("copy ")
        || line.starts_with("old mode")
        || line.starts_with("new mode")
        || line.starts_with("Binary files")
        || line.starts_with('\\')
    {
        DiffKind::Meta
    } else if line.starts_with('+') {
        DiffKind::Add
    } else if line.starts_with('-') {
        DiffKind::Del
    } else {
        DiffKind::Context
    }
}

/// Decide the dominant line terminator from raw bytes. Lone `\r` counts toward
/// CRLF because old Mac text and broken checkouts still show up in trees.
pub fn detect_line_ending(bytes: &[u8]) -> LineEnding {
    let mut crlf = 0usize;
    let mut lf = 0usize;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\r' {
            if i + 1 < bytes.len() && bytes[i + 1] == b'\n' {
                crlf += 1;
                i += 2;
            } else {
                crlf += 1;
                i += 1;
            }
        } else if bytes[i] == b'\n' {
            lf += 1;
            i += 1;
        } else {
            i += 1;
        }
    }
    if crlf > lf {
        LineEnding::CrLf
    } else {
        LineEnding::Lf
    }
}

/// Split UTF-8 text into lines without using [`str::lines`], which strips `\r`
/// and hides whether the file ended with a terminator.
pub fn split_text(s: &str) -> (Vec<String>, bool) {
    if s.is_empty() {
        return (Vec::new(), false);
    }
    let trailing_newline = s.ends_with("\r\n") || s.ends_with('\n');
    let mut lines = Vec::new();
    let mut start = 0usize;
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if i + 1 < bytes.len() && bytes[i] == b'\r' && bytes[i + 1] == b'\n' {
            lines.push(s[start..i].to_string());
            i += 2;
            start = i;
        } else if bytes[i] == b'\n' || bytes[i] == b'\r' {
            lines.push(s[start..i].to_string());
            i += 1;
            start = i;
        } else {
            i += 1;
        }
    }
    if start < bytes.len() {
        lines.push(s[start..].to_string());
    }
    (lines, trailing_newline)
}

/// Parse UTF-8 bytes into a [`TextFile`]. Returns `None` for non-UTF-8 input.
pub fn parse_text_bytes(bytes: &[u8]) -> Option<TextFile> {
    let s = String::from_utf8(bytes.to_vec()).ok()?;
    let ending = detect_line_ending(bytes);
    let (lines, trailing_newline) = split_text(&s);
    Some(TextFile {
        lines,
        ending,
        trailing_newline,
    })
}

// endregion: Pure parsers

// region: The shell wrappers
// ---------------------------------------------------------------------------
// The shell wrappers — thin; every failure is an empty answer, not a panic
// ---------------------------------------------------------------------------

/// Run `git -C <root> <args>`, returning stdout on success. Any failure —
/// git absent, not a repository, a non-zero exit — is `None`, which the
/// callers turn into an empty result and an honest note.
///
/// Stdio is piped, not inherited, so nothing from git reaches Emma's console.
fn git(root: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The repository's tracked files, relative to `root`, sorted. Prefers
/// `git ls-files`; if `root` is not a repository (or git is absent) it falls
/// back to a bounded filesystem walk.
pub fn paths(root: &Path) -> Vec<String> {
    if let Some(out) = git(root, &["ls-files"]) {
        let mut v: Vec<String> = out
            .lines()
            .filter(|l| !l.is_empty())
            .take(MAX_PATHS)
            .map(str::to_string)
            .collect();
        if !v.is_empty() {
            v.sort();
            v.dedup();
            return v;
        }
    }
    walk(root)
}

/// A file's commit history, newest first, following renames.
pub fn history(root: &Path, rel: &str) -> Vec<Commit> {
    let fmt = format!("--format=%H{US}%h{US}%an{US}%ad{US}%s");
    match git(root, &["log", "--follow", "--date=short", &fmt, "--", rel]) {
        Some(out) => parse_log(&out),
        None => Vec::new(),
    }
}

/// The diff a commit introduced to one file. `--format=` drops the commit
/// message so only the patch remains.
pub fn diff_at(root: &Path, hash: &str, rel: &str) -> Vec<DiffRow> {
    match git(root, &["show", "--format=", "-p", hash, "--", rel]) {
        Some(out) => parse_diff(&out),
        None => Vec::new(),
    }
}

/// Read a working-tree file for the viewer, or say why it cannot be shown.
pub fn read_file(path: &Path) -> FileRead {
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(e) => return FileRead::Refused(format!("cannot read: {e}")),
    };
    if meta.len() > MAX_FILE {
        return FileRead::Refused(format!("{} bytes — too large to show here", meta.len()));
    }
    match std::fs::read(path) {
        Ok(bytes) => {
            if bytes.contains(&0) {
                return FileRead::Refused("binary file".to_string());
            }
            match parse_text_bytes(&bytes) {
                Some(text) => FileRead::Text(text),
                None => FileRead::Refused("not UTF-8".to_string()),
            }
        }
        Err(e) => FileRead::Refused(format!("cannot read: {e}")),
    }
}

/// A bounded filesystem walk for the non-repository fallback: skips dotfiles,
/// `target/` and `node_modules/`, caps depth and total count.
fn walk(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    walk_into(root, root, 0, &mut out);
    out.sort();
    out.dedup();
    out
}

fn walk_into(root: &Path, dir: &Path, depth: usize, out: &mut Vec<String>) {
    if depth > 8 || out.len() >= MAX_PATHS {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in rd.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') || name == "target" || name == "node_modules" {
            continue;
        }
        let path = entry.path();
        if path.is_dir() {
            walk_into(root, &path, depth + 1, out);
        } else if let Ok(rel) = path.strip_prefix(root) {
            out.push(rel.to_string_lossy().replace('\\', "/"));
        }
        if out.len() >= MAX_PATHS {
            return;
        }
    }
}

/// The absolute path of a repo-relative file, for [`read_file`].
pub fn abs(root: &Path, rel: &str) -> PathBuf {
    root.join(rel)
}

/// The absolute path a repo-relative name resolves to, refusing anything that
/// could leave `root`.
///
/// The editor writes to the user's real tree, so this is a gate and not a
/// convenience: an absolute path, a Windows-style prefix, a root component or
/// any `..` is refused outright rather than normalised, and after that the
/// parent directory is canonicalised and must still sit inside the
/// canonicalised root. The second test is what catches a symlink pointing out
/// of the repository, which no amount of string checking would see.
pub fn resolve_in_root(root: &Path, rel: &str) -> Result<PathBuf, String> {
    use std::path::Component;

    if rel.is_empty() {
        return Err("no file to write".to_string());
    }
    let candidate = Path::new(rel);
    if candidate.is_absolute() {
        return Err(format!("{rel} is not a path inside this repository"));
    }
    for c in candidate.components() {
        match c {
            Component::Normal(_) | Component::CurDir => {}
            _ => return Err(format!("{rel} is not a path inside this repository")),
        }
    }
    let full = root.join(candidate);
    let root_real = root
        .canonicalize()
        .map_err(|e| format!("cannot resolve the repository root: {e}"))?;
    let parent = full
        .parent()
        .ok_or_else(|| format!("{rel} has no parent directory"))?;
    let parent_real = parent
        .canonicalize()
        .map_err(|e| format!("cannot resolve {}: {e}", parent.display()))?;
    if !parent_real.starts_with(&root_real) {
        return Err(format!("{rel} resolves outside this repository"));
    }
    let name = full
        .file_name()
        .ok_or_else(|| format!("{rel} does not name a file"))?;
    Ok(parent_real.join(name))
}

/// The bytes a buffer of lines becomes on disk, using the same terminator and
/// trailing-newline shape the file had when it was opened.
pub fn joined(lines: &[String], ending: LineEnding, trailing_newline: bool) -> String {
    if lines.is_empty() && !trailing_newline {
        return String::new();
    }
    let sep = ending.as_str();
    let mut s = lines.join(sep);
    if trailing_newline {
        s.push_str(sep);
    }
    s
}

/// Write an edited buffer back to exactly the file it was opened from.
///
/// Temp file beside the target and a rename over it, the `settings::save`
/// shape and for the same reason: a crash between the truncate and the last
/// byte of a direct write leaves the user's source file truncated. The rename
/// is atomic within a directory, so a reader sees the old file or the new one.
///
/// `expect` is the hash the buffer was opened with; a file whose bytes no
/// longer hash to it is refused untouched.
pub fn save_file(
    root: &Path,
    rel: &str,
    lines: &[String],
    ending: LineEnding,
    trailing_newline: bool,
    expect: u64,
) -> Saved {
    let path = match resolve_in_root(root, rel) {
        Ok(p) => p,
        Err(e) => return Saved::Refused(e),
    };
    let current = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) => return Saved::Refused(format!("cannot read {rel}: {e}")),
    };
    if hash_bytes(&current) != expect {
        return Saved::ChangedOnDisk;
    }
    let body = joined(lines, ending, trailing_newline);
    // A suffix, not `with_extension`: replacing the extension would make
    // `main.rs` and `main.py` share one temp name.
    let mut temp_name = path.file_name().unwrap_or_default().to_os_string();
    temp_name.push(".emma-tmp");
    let temp = path.with_file_name(temp_name);
    if let Err(e) = std::fs::write(&temp, body.as_bytes()) {
        return Saved::Refused(format!("cannot write {rel}: {e}"));
    }
    if let Err(e) = std::fs::rename(&temp, &path) {
        let _ = std::fs::remove_file(&temp);
        return Saved::Refused(format!("cannot write {rel}: {e}"));
    }
    Saved::Ok(hash_bytes(body.as_bytes()))
}

// endregion: The shell wrappers

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_log_reads_five_fields_per_record() {
        let stdout = format!(
            "{h1}{US}abc1234{US}Ada{US}2026-08-01{US}first commit\n\
             {h2}{US}def5678{US}Grace{US}2026-08-02{US}second: with, punctuation\n",
            h1 = "a".repeat(40),
            h2 = "b".repeat(40),
        );
        let got = parse_log(&stdout);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].short, "abc1234");
        assert_eq!(got[0].author, "Ada");
        assert_eq!(got[0].date, "2026-08-01");
        assert_eq!(got[0].subject, "first commit");
        assert_eq!(got[1].subject, "second: with, punctuation");
    }

    #[test]
    fn parse_log_drops_a_record_with_no_hash() {
        let stdout = format!("{US}{US}{US}{US}\n");
        assert!(parse_log(&stdout).is_empty());
        assert!(parse_log("").is_empty());
    }

    #[test]
    fn classify_puts_file_headers_before_bare_signs() {
        let diff = "diff --git a/x b/x\n\
                    index 111..222 100644\n\
                    --- a/x\n\
                    +++ b/x\n\
                    @@ -1,2 +1,2 @@\n\
                    -gone\n\
                    +added\n\
                     kept\n\
                    \\ No newline at end of file\n";
        let rows = parse_diff(diff);
        let kinds: Vec<DiffKind> = rows.iter().map(|r| r.kind).collect();
        assert_eq!(
            kinds,
            vec![
                DiffKind::Meta,    // diff --git
                DiffKind::Meta,    // index
                DiffKind::Meta,    // --- a/x  (header, not a removal)
                DiffKind::Meta,    // +++ b/x  (header, not an addition)
                DiffKind::Hunk,    // @@
                DiffKind::Del,     // -gone
                DiffKind::Add,     // +added
                DiffKind::Context, //  kept
                DiffKind::Meta,    // \ No newline
            ]
        );
    }

    #[test]
    fn empty_diff_is_empty_not_a_fabricated_row() {
        assert!(parse_diff("").is_empty());
    }

    #[test]
    fn detect_line_ending_counts_crlf_and_lf() {
        assert_eq!(detect_line_ending(b"a\r\nb\r\n"), LineEnding::CrLf);
        assert_eq!(detect_line_ending(b"a\nb\n"), LineEnding::Lf);
        assert_eq!(detect_line_ending(b"no breaks"), LineEnding::Lf);
    }

    #[test]
    fn split_text_records_trailing_newline() {
        let (lines, trailing) = split_text("a\nb\n");
        assert_eq!(lines, vec!["a", "b"]);
        assert!(trailing);
        let (lines, trailing) = split_text("a\nb");
        assert_eq!(lines, vec!["a", "b"]);
        assert!(!trailing);
        let (lines, trailing) = split_text("\n");
        assert_eq!(lines, vec![""]);
        assert!(trailing);
    }

    #[test]
    fn joined_round_trips_lf_with_and_without_trailing_newline() {
        assert_eq!(joined(&[], LineEnding::Lf, false), "");
        assert_eq!(
            joined(&["a".to_string(), "b".to_string()], LineEnding::Lf, true),
            "a\nb\n"
        );
        assert_eq!(
            joined(&["a".to_string(), "b".to_string()], LineEnding::Lf, false),
            "a\nb"
        );
    }

    #[test]
    fn joined_round_trips_crlf() {
        assert_eq!(
            joined(&["a".to_string(), "b".to_string()], LineEnding::CrLf, true),
            "a\r\nb\r\n"
        );
    }

    // -----------------------------------------------------------------
    // Save, containment, line-ending round trips, and a throwaway repo.
    // Local `git` in a TempDir is a subprocess, not a network: offline.
    // -----------------------------------------------------------------

    use std::path::Path;
    use tempfile::TempDir;

    fn run(dir: &Path, args: &[&str]) {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .expect("git must be on PATH for these tests");
        assert!(out.status.success(), "git {args:?} failed: {out:?}");
    }

    /// A repository with one file and two commits touching it.
    fn repo() -> TempDir {
        let td = TempDir::new().unwrap();
        let dir = td.path();
        run(dir, &["init", "--initial-branch=main"]);
        run(dir, &["config", "user.email", "t@example.com"]);
        run(dir, &["config", "user.name", "T"]);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/lib.rs"), "one\n").unwrap();
        run(dir, &["add", "."]);
        run(dir, &["commit", "-m", "first: add lib"]);
        std::fs::write(dir.join("src/lib.rs"), "one\ntwo\n").unwrap();
        run(dir, &["add", "."]);
        run(dir, &["commit", "-m", "second: add a line"]);
        td
    }

    fn round_trip_bytes(path: &Path, original: &[u8]) {
        let FileRead::Text(text) = read_file(path) else {
            panic!("expected text file");
        };
        let open_hash = file_hash(path).unwrap();
        let out = save_file(
            path.parent().unwrap(),
            path.file_name().unwrap().to_str().unwrap(),
            &text.lines,
            text.ending,
            text.trailing_newline,
            open_hash,
        );
        let Saved::Ok(_) = out else {
            panic!("expected save ok, got {out:?}");
        };
        assert_eq!(
            std::fs::read(path).unwrap(),
            original,
            "bytes must round-trip unchanged"
        );
    }

    #[test]
    fn a_crlf_file_saved_unchanged_is_byte_identical() {
        let td = TempDir::new().unwrap();
        let path = td.path().join("crlf.txt");
        let original = b"line one\r\nline two\r\n";
        std::fs::write(&path, original).unwrap();
        round_trip_bytes(&path, original);
    }

    #[test]
    fn an_lf_file_saved_unchanged_is_byte_identical() {
        let td = TempDir::new().unwrap();
        let path = td.path().join("lf.txt");
        let original = b"line one\nline two\n";
        std::fs::write(&path, original).unwrap();
        round_trip_bytes(&path, original);
    }

    #[test]
    fn a_file_with_no_trailing_newline_saved_unchanged_is_byte_identical() {
        let td = TempDir::new().unwrap();
        let path = td.path().join("no_nl.txt");
        let original = b"line one\nline two";
        std::fs::write(&path, original).unwrap();
        round_trip_bytes(&path, original);
    }

    #[test]
    fn history_and_diff_come_back_for_a_real_repository() {
        let td = repo();
        let commits = history(td.path(), "src/lib.rs");
        assert_eq!(commits.len(), 2, "{commits:?}");
        assert_eq!(commits[0].subject, "second: add a line");
        assert_eq!(commits[1].subject, "first: add lib");
        // The newest commit's patch for this file adds exactly "two".
        let rows = diff_at(td.path(), &commits[0].hash, "src/lib.rs");
        assert!(
            rows.iter()
                .any(|r| r.kind == DiffKind::Add && r.text == "+two"),
            "{rows:?}"
        );
        // The older commit's patch is a different one, which is the whole
        // point of selecting a commit.
        let older = diff_at(td.path(), &commits[1].hash, "src/lib.rs");
        assert!(
            older
                .iter()
                .any(|r| r.kind == DiffKind::Add && r.text == "+one"),
            "{older:?}"
        );
        assert!(!older.iter().any(|r| r.text == "+two"), "{older:?}");
    }

    #[test]
    fn a_file_with_no_history_reports_none_rather_than_inventing_rows() {
        let td = repo();
        std::fs::write(td.path().join("untracked.txt"), "hi\n").unwrap();
        assert!(history(td.path(), "untracked.txt").is_empty());
    }

    #[test]
    fn resolve_in_root_refuses_to_leave_the_repository() {
        let td = TempDir::new().unwrap();
        std::fs::write(td.path().join("in.txt"), "x").unwrap();
        assert!(resolve_in_root(td.path(), "in.txt").is_ok());
        for bad in ["../out.txt", "src/../../out.txt", "/etc/passwd", ""] {
            assert!(
                resolve_in_root(td.path(), bad).is_err(),
                "{bad} was allowed out of the root"
            );
        }
    }

    #[test]
    fn save_file_writes_atomically_and_leaves_no_temp_behind() {
        let td = TempDir::new().unwrap();
        std::fs::write(td.path().join("a.rs"), "one\n").unwrap();
        let open_hash = file_hash(&td.path().join("a.rs")).unwrap();
        let lines = vec!["one".to_string(), "two".to_string()];
        let out = save_file(td.path(), "a.rs", &lines, LineEnding::Lf, true, open_hash);
        let Saved::Ok(new_hash) = out else {
            panic!("expected a write, got {out:?}");
        };
        assert_eq!(
            std::fs::read_to_string(td.path().join("a.rs")).unwrap(),
            "one\ntwo\n"
        );
        assert_eq!(new_hash, file_hash(&td.path().join("a.rs")).unwrap());
        let leftovers: Vec<String> = std::fs::read_dir(td.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains("emma-tmp"))
            .collect();
        assert!(leftovers.is_empty(), "temp file left behind: {leftovers:?}");
    }

    #[test]
    fn save_file_refuses_a_file_that_changed_on_disk_and_writes_nothing() {
        let td = TempDir::new().unwrap();
        std::fs::write(td.path().join("a.rs"), "one\n").unwrap();
        let open_hash = file_hash(&td.path().join("a.rs")).unwrap();
        // Somebody else rewrites it while the buffer is open.
        std::fs::write(td.path().join("a.rs"), "somebody else\n").unwrap();
        let out = save_file(
            td.path(),
            "a.rs",
            &["mine".to_string()],
            LineEnding::Lf,
            true,
            open_hash,
        );
        assert_eq!(out, Saved::ChangedOnDisk);
        assert_eq!(
            std::fs::read_to_string(td.path().join("a.rs")).unwrap(),
            "somebody else\n",
            "the other edit must survive a refused save"
        );
    }

    #[test]
    fn save_file_refuses_a_path_outside_the_root() {
        let td = TempDir::new().unwrap();
        let out = save_file(
            td.path(),
            "../escape.txt",
            &["x".to_string()],
            LineEnding::Lf,
            true,
            0,
        );
        assert!(matches!(out, Saved::Refused(_)), "{out:?}");
        assert!(!td.path().parent().unwrap().join("escape.txt").exists());
    }

    /// Certification against the checkout that contains this crate. Output is
    /// captured in `target/port/DONE-G1.md`; the assertions keep it from rotting.
    #[test]
    fn certify_against_the_checkout_repository() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let root = root.canonicalize().expect("repository root");
        let files = paths(&root);
        assert!(!files.is_empty(), "ls-files or walk must return paths");
        let sample = "crates/emma/src/term/code_git.rs";
        assert!(
            files.iter().any(|p| p == sample),
            "{sample} not in tree: {:?}",
            &files[..5.min(files.len())]
        );
        let commits = history(&root, sample);
        assert!(!commits.is_empty(), "history for {sample}");
        let rows = diff_at(&root, &commits[0].hash, sample);
        assert!(!rows.is_empty(), "diff for newest commit on {sample}");
        eprintln!(
            "certify paths (first 5): {:?}",
            &files[..5.min(files.len())]
        );
        eprintln!(
            "certify history[0]: {} {} {} {}",
            commits[0].short, commits[0].author, commits[0].date, commits[0].subject
        );
        eprintln!(
            "certify diff rows (first 8 kinds): {:?}",
            rows.iter()
                .take(8)
                .map(|r| (&r.kind, &r.text))
                .collect::<Vec<_>>()
        );
    }
}
