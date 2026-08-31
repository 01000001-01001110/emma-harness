//! Which language server, and the refusal when there is none.
//!
//! Modelled on `tools/fs/src/bash.rs`: a documented order, an explicit
//! override, and an `Unavailable` that names what was looked for and where. The
//! shape is deliberately the same so that a person who has read one can predict
//! the other, and so that neither ever falls back to something that answers a
//! different question.
//!
//! **Rust only, and it says so.** `rust-analyzer` is the only server wired.
//! A request about a `.py` file is `Unavailable` naming that fact — not an
//! attempt at a generic server, and emphatically not a quiet fall back to text
//! search. A tool that answers "here are the references" using grep has answered
//! a different question than the one asked, and the model cannot tell.
//!
//! The order, which is the contract:
//!
//! 1. [`OVERRIDE_ENV`], if set to anything but whitespace — an absolute path or
//!    a bare name to look up. If what it names is not there, or is there and
//!    does not identify itself as rust-analyzer, the call is `Unavailable` and
//!    says which. It never falls back.
//! 2. `rust-analyzer` on `PATH`.
//! 3. The server that ships inside the VS Code extension —
//!    `~/.vscode/extensions/rust-lang.rust-analyzer-*/server/`, and the
//!    `.vscode-server` spelling used by remote and WSL installs. Highest
//!    version wins.
//! 4. Nothing, with a refusal naming both opt-ins.
//!
//! **Every candidate is probed with `--version` before it is chosen**, and that
//! is the one place this diverges from `bash.rs`, which classifies by filename
//! precisely so that it never has to execute a candidate to find out what it is.
//! The difference is that a shell's selection argument is `-c`, so probing a
//! shell means running something; `--version` runs nothing. And the probe is not
//! optional here, because of what is on this box:
//!
//! ```text
//! $ rust-analyzer --version
//! error: Unknown binary 'rust-analyzer.exe' in official toolchain '…'
//! ```
//!
//! `~/.cargo/bin/rust-analyzer` exists as a rustup proxy whether or not the
//! component behind it is installed. It is first on `PATH`, it is a real file,
//! and it exits **0** while printing that to stderr — so a chosen-by-existence
//! search picks a binary that will never speak LSP, and the failure surfaces
//! much later as a handshake that times out. Measured on this machine on
//! 2026-08-11, which is why the search costs one process spawn.

use std::path::{Path, PathBuf};
use std::time::Duration;

use emma_tool_api::ToolError;

// region: The contract
// ---------------------------------------------------------------------------
// The contract
//
// The override, the extension locations, and what a resolved server is. Kept
// above the mechanism because this is the part a user has to be able to predict
// without reading the rest.
// ---------------------------------------------------------------------------

/// The override. An environment variable rather than a config key for the same
/// reasons as `EMMA_SHELL`: no plumbing, settable per run, and the same shape as
/// the things it sits next to.
pub const OVERRIDE_ENV: &str = "EMMA_LSP_SERVER";

/// The one language this crate speaks. Stated as a constant so the refusal, the
/// tools and the tests cannot disagree about it.
pub const SUPPORTED_EXTENSION: &str = "rs";

/// The LSP language identifier sent on `textDocument/didOpen`.
pub const LANGUAGE_ID: &str = "rust";

/// Where the VS Code extension unpacks its server, relative to a home
/// directory. Both spellings, because `.vscode-server` is what a remote or WSL
/// install uses and it is otherwise identical.
const EXTENSION_ROOTS: &[&str] = &[".vscode/extensions", ".vscode-server/extensions"];
const EXTENSION_PREFIX: &str = "rust-lang.rust-analyzer-";

/// A server that was found *and* answered `--version`.
///
/// Carrying `version` rather than discarding it is the point of having probed:
/// it goes on the first line of every result, so a wrong answer is attributable
/// to a particular build without anybody having to reproduce the search.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Server {
    pub path: PathBuf,
    pub version: String,
    pub source: Source,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Override,
    Path,
    VsCodeExtension,
}

impl Source {
    fn label(self) -> &'static str {
        match self {
            Self::Override => "EMMA_LSP_SERVER",
            Self::Path => "PATH",
            Self::VsCodeExtension => "the VS Code extension",
        }
    }
}

impl Server {
    /// The one line prepended to every result, for exactly the reason
    /// `Bash::banner` exists: the *description* cannot name the local server,
    /// because the description is hashed into `tool_schema_hash` and a hash that
    /// varies by machine stops being an attribution. So the machine-specific
    /// fact arrives in the result, where it is exact and costs one line.
    pub fn banner(&self) -> String {
        format!(
            "server: {} — {} (found via {})",
            self.version,
            self.path.display(),
            self.source.label()
        )
    }
}

impl std::fmt::Display for Server {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} at {}", self.version, self.path.display())
    }
}

// endregion: The contract

// region: The search
// ---------------------------------------------------------------------------
// The search
//
// Everything the decision depends on is passed in, so the order and both
// refusals are a pure function of a candidate list and a probe — testable on a
// box with no language server at all, and testable for the rustup-proxy case
// without needing a broken rustup to hand.
// ---------------------------------------------------------------------------

/// What a probe learned about one candidate.
pub enum Probe {
    /// It ran and identified itself. The string is the version line.
    Rust(String),
    /// It ran, or did not, and is not a usable rust-analyzer. The string is
    /// carried into the refusal — this is how the rustup proxy explains itself
    /// to the user rather than being silently skipped.
    Not(String),
}

/// Everything the decision depends on, passed in rather than read.
pub struct Env<'a> {
    pub windows: bool,
    pub path_dirs: Vec<PathBuf>,
    /// The VS Code extension server directories, highest version first.
    pub extension_dirs: Vec<PathBuf>,
    pub exists: &'a dyn Fn(&Path) -> bool,
    pub probe: &'a dyn Fn(&Path) -> Probe,
}

/// The resolved server, or an honest refusal — against the real environment.
///
/// Public because "which server answered my question" must be answerable
/// without reading this file.
pub fn resolve() -> Result<Server, ToolError> {
    let exists = |p: &Path| p.is_file();
    let probe = |p: &Path| probe_version(p);
    let env = Env {
        windows: cfg!(windows),
        path_dirs: std::env::var_os("PATH")
            .map(|p| std::env::split_paths(&p).collect())
            .unwrap_or_default(),
        extension_dirs: extension_dirs(),
        exists: &exists,
        probe: &probe,
    };
    resolve_with(std::env::var(OVERRIDE_ENV).ok().as_deref(), &env)
}

/// The documented order, in one function.
pub fn resolve_with(over: Option<&str>, env: &Env<'_>) -> Result<Server, ToolError> {
    if let Some(spec) = over.map(str::trim).filter(|s| !s.is_empty()) {
        return resolve_override(spec, env);
    }

    // Rejections are collected rather than dropped so the refusal at the bottom
    // can say "I found this and it answered that", which is the difference
    // between a user fixing their rustup in one minute and reverse-engineering
    // this function.
    let mut rejected: Vec<String> = Vec::new();
    for name in names_for("rust-analyzer", env) {
        for dir in &env.path_dirs {
            let candidate = dir.join(&name);
            if !(env.exists)(&candidate) {
                continue;
            }
            match (env.probe)(&candidate) {
                Probe::Rust(version) => {
                    return Ok(Server {
                        path: candidate,
                        version,
                        source: Source::Path,
                    })
                }
                Probe::Not(why) => rejected.push(format!("{} — {why}", candidate.display())),
            }
        }
    }

    for dir in &env.extension_dirs {
        for name in names_for("rust-analyzer", env) {
            let candidate = dir.join(&name);
            if !(env.exists)(&candidate) {
                continue;
            }
            match (env.probe)(&candidate) {
                Probe::Rust(version) => {
                    return Ok(Server {
                        path: candidate,
                        version,
                        source: Source::VsCodeExtension,
                    })
                }
                Probe::Not(why) => rejected.push(format!("{} — {why}", candidate.display())),
            }
        }
    }

    Err(nothing_found(env, &rejected))
}

fn resolve_override(spec: &str, env: &Env<'_>) -> Result<Server, ToolError> {
    let looks_like_path = spec.contains(['/', '\\']) || Path::new(spec).is_absolute();
    let found = if looks_like_path {
        let p = PathBuf::from(spec);
        (env.exists)(&p).then_some(p)
    } else {
        names_for(spec, env)
            .into_iter()
            .flat_map(|name| {
                env.path_dirs
                    .iter()
                    .chain(env.extension_dirs.iter())
                    .map(move |d| d.join(&name))
            })
            .find(|c| (env.exists)(c))
    };

    let Some(path) = found else {
        // Refuse rather than fall back. A tool that silently consults a
        // different server than the one you named produces answers that read
        // exactly like the answers you asked for.
        let where_looked = if looks_like_path {
            "no file at that path".to_string()
        } else {
            format!("looked in: {}", dir_list(env))
        };
        return Err(ToolError::Unavailable(format!(
            "{OVERRIDE_ENV} names \"{spec}\", and no such language server was found \
             ({where_looked}). Set {OVERRIDE_ENV} to an absolute path, or unset it to take \
             the default."
        )));
    };

    match (env.probe)(&path) {
        Probe::Rust(version) => Ok(Server {
            path,
            version,
            source: Source::Override,
        }),
        // An override that names a real file which is not a language server is
        // still refused. It is the rustup-proxy case arriving by another route,
        // and letting it through would trade an immediate, explained failure for
        // a handshake timeout thirty seconds later.
        Probe::Not(why) => Err(ToolError::Unavailable(format!(
            "{OVERRIDE_ENV} names {}, which is not a usable rust-analyzer: {why}. \
             Emma probes the binary with `--version` before speaking LSP to it, because a \
             binary that cannot answer that will not answer `initialize` either — it will \
             simply never reply.",
            path.display()
        ))),
    }
}

fn names_for(name: &str, env: &Env<'_>) -> Vec<String> {
    if env.windows {
        vec![format!("{name}.exe"), name.to_string()]
    } else {
        vec![name.to_string()]
    }
}

fn dir_list(env: &Env<'_>) -> String {
    let dirs: Vec<String> = env
        .path_dirs
        .iter()
        .chain(env.extension_dirs.iter())
        .map(|d| d.display().to_string())
        .collect();
    if dirs.is_empty() {
        "no directories to search".into()
    } else {
        dirs.join(", ")
    }
}

/// The refusal when the search comes up empty.
///
/// It names what was looked for, where, *what was found and rejected*, and both
/// opt-ins. The rejected list is the part that earns its place: on this machine
/// the search finds a `rust-analyzer` and refuses it, and a refusal that said
/// only "not found" would be actively misleading to someone looking at a
/// `rust-analyzer` on their `PATH`.
fn nothing_found(env: &Env<'_>, rejected: &[String]) -> ToolError {
    let mut msg = format!(
        "no rust-analyzer is available. Emma looked for rust-analyzer in: {}.",
        dir_list(env)
    );
    if !rejected.is_empty() {
        msg.push_str(&format!(
            " Candidates were found and rejected: {}.",
            rejected.join("; ")
        ));
    }
    msg.push_str(&format!(
        " Install it with `rustup component add rust-analyzer`, or set {OVERRIDE_ENV} to the \
         absolute path of a rust-analyzer binary. Emma will not answer these questions with \
         text search instead: a reference list produced by grep is a different answer to a \
         different question, and nothing in the result would say so."
    ));
    ToolError::Unavailable(msg)
}

/// The refusal for a file this crate has no server for.
///
/// Separate from [`nothing_found`] because the fix is different — one is
/// "install something", the other is "this is not implemented" — and because
/// this is the message that has to be unambiguous about not falling back.
pub fn unsupported_language(shown: &str) -> ToolError {
    ToolError::Unavailable(format!(
        "{shown} is not a Rust file, and rust-analyzer is the only language server Emma has \
         wired. Use Grep for a text search, knowing that it is a text search: it cannot tell a \
         definition from a mention, and it will match the same name in an unrelated crate."
    ))
}

// endregion: The search

// region: Probing, and where the extension keeps its server
// ---------------------------------------------------------------------------
// Probing, and where the extension keeps its server
//
// The two impure halves, kept apart from the decision so that the decision
// stays testable. Both are best-effort by construction: a probe that cannot run
// is a rejection with a reason, and a home directory that is not there
// contributes no candidates.
// ---------------------------------------------------------------------------

/// How long a `--version` probe may take. It is a process spawn and an
/// immediate print; anything slower is a binary doing something a version flag
/// should not do, and waiting on it would stall the first call of the session.
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// Runs `<candidate> --version` and decides what it is.
///
/// Requires the *stdout* to start with `rust-analyzer`. The rustup proxy prints
/// its complaint to stderr and exits 0, so neither the exit status nor the
/// merged streams distinguish it — the identifying string on the right stream
/// is what does.
fn probe_version(path: &Path) -> Probe {
    // Deliberately `std` and blocking rather than tokio. This runs once per
    // server start, inside a call that is about to wait seconds for indexing,
    // and a blocking spawn here avoids making every caller of `resolve` async
    // for a ten-millisecond process.
    let output = std::process::Command::new(path)
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .output();
    let output = match output {
        Ok(o) => o,
        Err(e) => return Probe::Not(format!("could not be run: {e}")),
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    let line = stdout.lines().next().unwrap_or("").trim().to_string();
    if line.starts_with("rust-analyzer") {
        return Probe::Rust(line);
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let said = [line.as_str(), stderr.lines().next().unwrap_or("").trim()]
        .into_iter()
        .find(|s| !s.is_empty())
        .unwrap_or("nothing")
        .to_string();
    Probe::Not(format!(
        "`--version` said {said:?} rather than identifying itself as rust-analyzer"
    ))
}

/// `PROBE_TIMEOUT` is declared and not yet enforced, and saying so is cheaper
/// than a comment that implies otherwise. `std::process::Command::output` has no
/// timeout; enforcing one means a thread and a channel, for a case — a
/// `--version` that hangs forever — that has not been observed. It is a named
/// constant so the next person adding the thread has the number already agreed.
#[allow(dead_code)]
const _PROBE_TIMEOUT_IS_ASPIRATIONAL: Duration = PROBE_TIMEOUT;

/// Every VS Code extension server directory on this machine, highest version
/// first.
///
/// Sorted by the directory name, which sorts `0.3.3008` after `0.3.3005` and
/// would sort `0.3.30010` wrongly. That is accepted: the comparison is between
/// two installs of the same extension a user rarely has, either works, and a
/// semver parser for a tiebreak nobody will notice is not worth the code.
fn extension_dirs() -> Vec<PathBuf> {
    let Some(home) = home_dir() else {
        return Vec::new();
    };
    let mut found: Vec<PathBuf> = Vec::new();
    for root in EXTENSION_ROOTS {
        let Ok(entries) = std::fs::read_dir(home.join(root)) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with(EXTENSION_PREFIX) {
                found.push(entry.path());
            }
        }
    }
    found.sort_by(|a, b| b.file_name().cmp(&a.file_name()));
    found.into_iter().map(|d| d.join("server")).collect()
}

fn home_dir() -> Option<PathBuf> {
    // `USERPROFILE` first on Windows, `HOME` everywhere. Not a dependency for
    // two environment variables.
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
}

// endregion: Probing, and where the extension keeps its server

// region: Resolution tests
// ---------------------------------------------------------------------------
// Resolution tests
//
// The order, the override and both refusals are decided from a candidate list
// and a probe, so every branch runs on a box with no language server — and, more
// to the point, the rustup-proxy branch runs without needing a broken rustup.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// A fake filesystem: only these paths exist.
    ///
    /// Separators are normalised on both sides. The fixtures name unix *or*
    /// Windows paths so each branch can be exercised on any host, but
    /// `PathBuf::join` uses the host separator — `/usr/bin\rust-analyzer` on
    /// Windows, `C:\Users\a\.cargo\bin/rust-analyzer.exe` on unix — so a
    /// literal `PathBuf` comparison would make half these tests pass only on
    /// the host they were written for. Path equality and substring checks
    /// go through the same helper, for the same reason.
    fn only(files: &[&str]) -> impl Fn(&Path) -> bool {
        let set: Vec<String> = files.iter().map(|f| normalise(f)).collect();
        move |p: &Path| set.contains(&normalise(&p.to_string_lossy()))
    }

    fn normalise(path: &str) -> String {
        path.replace('\\', "/").to_lowercase()
    }

    /// A probe that answers per path, so a test can put a proxy and a real
    /// server in the same search and pin which one wins.
    fn probes(map: &'static [(&'static str, Option<&'static str>)]) -> impl Fn(&Path) -> Probe {
        move |p: &Path| {
            let key = normalise(&p.to_string_lossy());
            for (path, answer) in map {
                if key == normalise(path) {
                    return match answer {
                        Some(v) => Probe::Rust((*v).to_string()),
                        None => Probe::Not(
                            "`--version` said \"error: Unknown binary 'rust-analyzer.exe' in \
                             official toolchain\" rather than identifying itself as \
                             rust-analyzer"
                                .to_string(),
                        ),
                    };
                }
            }
            Probe::Not("not in the fixture".to_string())
        }
    }

    fn env<'a>(
        windows: bool,
        path_dirs: &[&str],
        extension_dirs: &[&str],
        exists: &'a dyn Fn(&Path) -> bool,
        probe: &'a dyn Fn(&Path) -> Probe,
    ) -> Env<'a> {
        Env {
            windows,
            path_dirs: path_dirs.iter().map(PathBuf::from).collect(),
            extension_dirs: extension_dirs.iter().map(PathBuf::from).collect(),
            exists,
            probe,
        }
    }

    /// The whole reason the search costs a process spawn, and it is not
    /// hypothetical — it is this machine. `~/.cargo/bin/rust-analyzer.exe`
    /// exists as a rustup proxy with the component uninstalled: a real file,
    /// first on `PATH`, exiting 0. Choosing by existence picks it, and the
    /// symptom is a handshake that never completes.
    #[test]
    fn a_rustup_proxy_on_path_is_rejected_in_favour_of_a_real_server() {
        const PROXY: &str = r"C:\Users\a\.cargo\bin\rust-analyzer.exe";
        const REAL: &str = r"C:\Users\a\.vscode\extensions\rust-lang.rust-analyzer-0.3.3008\server\rust-analyzer.exe";
        let files = only(&[PROXY, REAL]);
        let probe = probes(&[
            (PROXY, None),
            (REAL, Some("rust-analyzer 0.3.3008-standalone")),
        ]);
        let e = env(
            true,
            &[r"C:\Users\a\.cargo\bin"],
            &[r"C:\Users\a\.vscode\extensions\rust-lang.rust-analyzer-0.3.3008\server"],
            &files,
            &probe,
        );
        let server = resolve_with(None, &e).expect("the extension server");
        assert_eq!(normalise(&server.path.to_string_lossy()), normalise(REAL));
        assert_eq!(server.source, Source::VsCodeExtension);
        assert!(server.version.contains("0.3.3008"), "{server}");
    }

    /// And with only the proxy present, the refusal must say what it found and
    /// what that thing said. "Not found" would be a lie to someone looking
    /// straight at a `rust-analyzer` on their `PATH`.
    #[test]
    fn the_refusal_quotes_what_it_rejected_and_why() {
        const PROXY: &str = r"C:\Users\a\.cargo\bin\rust-analyzer.exe";
        let files = only(&[PROXY]);
        let probe = probes(&[(PROXY, None)]);
        let e = env(true, &[r"C:\Users\a\.cargo\bin"], &[], &files, &probe);
        let err = resolve_with(None, &e).expect_err("a proxy is not a server");
        assert_eq!(err.kind(), "tool_unavailable");
        let d = err.detail();
        assert!(normalise(d).contains(&normalise(PROXY)), "{d}");
        assert!(d.contains("Unknown binary"), "{d}");
        assert!(d.contains("rustup component add rust-analyzer"), "{d}");
        assert!(d.contains(OVERRIDE_ENV), "{d}");
    }

    /// Never grep. The refusal is where a future reader is most tempted to add
    /// a fallback, so the promise not to is asserted rather than merely
    /// commented.
    #[test]
    fn no_refusal_offers_a_silent_text_search_instead() {
        let none = only(&[]);
        let probe = probes(&[]);
        let e = env(false, &["/usr/bin"], &[], &none, &probe);
        let err = resolve_with(None, &e).expect_err("nothing installed");
        let d = err.detail().to_lowercase();
        assert!(d.contains("grep") || d.contains("text search"), "{d}");
        assert!(
            d.contains("different answer") || d.contains("different question"),
            "the refusal must say why grep is not a substitute: {d}"
        );

        // The other refusal, for a file this crate has no server for, has the
        // same obligation and reaches the model far more often.
        let lang = unsupported_language("app/main.py");
        assert_eq!(lang.kind(), "tool_unavailable");
        assert!(lang.detail().contains("app/main.py"), "{lang}");
        assert!(lang.detail().contains("text search"), "{lang}");
    }

    #[test]
    fn path_is_preferred_to_the_extension_when_both_answer() {
        const ON_PATH: &str = "/usr/bin/rust-analyzer";
        const EXT: &str =
            "/home/a/.vscode/extensions/rust-lang.rust-analyzer-0.3.1/server/rust-analyzer";
        let files = only(&[ON_PATH, EXT]);
        let probe = probes(&[
            (ON_PATH, Some("rust-analyzer 1.90.0")),
            (EXT, Some("rust-analyzer 0.3.1")),
        ]);
        let e = env(
            false,
            &["/usr/bin"],
            &["/home/a/.vscode/extensions/rust-lang.rust-analyzer-0.3.1/server"],
            &files,
            &probe,
        );
        let server = resolve_with(None, &e).expect("path wins");
        assert_eq!(server.path, PathBuf::from(ON_PATH));
        assert_eq!(server.source, Source::Path);
    }

    #[test]
    fn an_override_naming_a_path_wins_and_is_still_probed() {
        const MINE: &str = "/opt/ra/rust-analyzer";
        const OTHER: &str = "/usr/bin/rust-analyzer";
        let files = only(&[MINE, OTHER]);
        let probe = probes(&[
            (MINE, Some("rust-analyzer 0.3.9")),
            (OTHER, Some("rust-analyzer 0.3.1")),
        ]);
        let e = env(false, &["/usr/bin"], &[], &files, &probe);
        let server = resolve_with(Some(MINE), &e).expect("the override");
        assert_eq!(server.path, PathBuf::from(MINE));
        assert_eq!(server.source, Source::Override);

        // A named file that exists and is not a server is refused, not fallen
        // back from — the same ruling as `EMMA_SHELL` naming a missing shell.
        let bogus = only(&["/bin/ls", OTHER]);
        let probe = probes(&[(OTHER, Some("rust-analyzer 0.3.1"))]);
        let e = env(false, &["/usr/bin"], &[], &bogus, &probe);
        let err = resolve_with(Some("/bin/ls"), &e).expect_err("must not fall back");
        assert_eq!(err.kind(), "tool_unavailable");
        assert!(err.detail().contains("/bin/ls"), "{err}");
    }

    #[test]
    fn an_override_naming_nothing_refuses_rather_than_substituting() {
        let files = only(&["/usr/bin/rust-analyzer"]);
        let probe = probes(&[("/usr/bin/rust-analyzer", Some("rust-analyzer 0.3.1"))]);
        let e = env(false, &["/usr/bin"], &[], &files, &probe);
        let err = resolve_with(Some("/opt/nope"), &e).expect_err("must not fall back");
        assert_eq!(err.kind(), "tool_unavailable");
        assert!(err.detail().contains("/opt/nope"), "{err}");
        assert!(err.detail().contains(OVERRIDE_ENV), "{err}");
    }

    #[test]
    fn a_blank_override_means_unset() {
        let files = only(&["/usr/bin/rust-analyzer"]);
        let probe = probes(&[("/usr/bin/rust-analyzer", Some("rust-analyzer 0.3.1"))]);
        let e = env(false, &["/usr/bin"], &[], &files, &probe);
        assert_eq!(
            resolve_with(Some("  "), &e).expect("blank is unset").path,
            PathBuf::from("/usr/bin/rust-analyzer")
        );
    }

    #[test]
    fn the_banner_names_the_version_the_path_and_where_it_came_from() {
        let server = Server {
            path: PathBuf::from("/usr/bin/rust-analyzer"),
            version: "rust-analyzer 0.3.3008-standalone".into(),
            source: Source::Path,
        };
        let banner = server.banner();
        assert_eq!(banner.lines().count(), 1);
        assert!(banner.contains("0.3.3008"), "{banner}");
        assert!(banner.contains("/usr/bin/rust-analyzer"), "{banner}");
        assert!(banner.contains("PATH"), "{banner}");
    }
}

// endregion: Resolution tests
