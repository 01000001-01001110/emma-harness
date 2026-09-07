//! Which language server, and the refusal when there is none.
//!
//! Modelled on `tools/fs/src/bash.rs`: a documented order, an explicit
//! override, and an `Unavailable` that names what was looked for and where. The
//! shape is deliberately the same so that a person who has read one can predict
//! the other, and so that neither ever falls back to something that answers a
//! different question.
//!
//! **Seven languages, one search.** The languages live in [`crate::lang`] as
//! data. Nothing in this file knows what rust-analyzer is; it knows how to turn
//! a [`Language`] into a running command line, or into a refusal that names the
//! binary, the launcher, the directories searched and the install command.
//!
//! The order, which is the contract, run per language:
//!
//! 1. The override, if set to anything but whitespace: an absolute path or a
//!    bare name to look up. If what it names is not there, the call is
//!    `Unavailable` and says so. It never falls back. The variable is
//!    `EMMA_LSP_SERVER_<KEY>`, and `EMMA_LSP_SERVER` is still read for rust so
//!    that everything written about it stays true.
//! 2. Each candidate's binary name on `PATH`, in the table's order.
//! 3. Each candidate's editor-extension location, highest version first.
//! 4. Nothing, with a refusal naming every route.
//!
//! **Some candidates are probed with `--version` before they are chosen**, and
//! that is the one place this diverges from `bash.rs`, which classifies by
//! filename precisely so that it never has to execute a candidate to find out
//! what it is. The probe is not optional for rust, because of what was on the
//! Windows box on 2026-09-06 before the component was installed:
//!
//! ```text
//! $ rust-analyzer --version
//! error: Unknown binary 'rust-analyzer.exe' in official toolchain
//! '1.94.1-x86_64-pc-windows-msvc'.
//! ```
//!
//! `~/.cargo/bin/rust-analyzer` exists as a rustup proxy whether or not the
//! component behind it is installed. It is first on `PATH`, it is a real file,
//! and a chosen-by-existence search picks a binary that will never speak LSP,
//! so the failure surfaces much later as a handshake that times out. Which
//! candidates are probed, and for what, is [`crate::lang::Identity`], and the
//! reason a bundled `server.js` is *not* probed is recorded there.

use std::path::{Path, PathBuf};
use std::time::Duration;

use emma_tool_api::ToolError;

use crate::lang::{self, Candidate, Identity, Language, Launcher};

// region: The contract
// ---------------------------------------------------------------------------
// The contract
//
// The override, the extension locations, and what a resolved server is. Kept
// above the mechanism because this is the part a user has to be able to predict
// without reading the rest.
// ---------------------------------------------------------------------------

/// The override for rust, kept under its original name.
///
/// An environment variable rather than a config key for the same reasons as
/// `EMMA_SHELL`: no plumbing, settable per run, and the same shape as the things
/// it sits next to. Every other language uses [`override_env`].
pub const OVERRIDE_ENV: &str = "EMMA_LSP_SERVER";

/// The override variable for one language, for example
/// `EMMA_LSP_SERVER_TERRAFORM`.
pub fn override_env(language: &Language) -> String {
    format!("EMMA_LSP_SERVER_{}", language.key.to_ascii_uppercase())
}

/// Where VS Code unpacks extensions, relative to a home directory. Both
/// spellings, because `.vscode-server` is what a remote or WSL install uses and
/// it is otherwise identical.
const EXTENSION_ROOTS: &[&str] = &[".vscode/extensions", ".vscode-server/extensions"];

/// A server that was found, and whose launcher was found too.
///
/// Carrying `version` rather than discarding it is the point of having probed:
/// it goes on the first line of every result, so a wrong answer is attributable
/// to a particular build without anybody having to reproduce the search. When
/// the candidate's [`Identity`] is `LauncherOnly` there is no version to carry
/// and the field says so, which is itself worth printing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Server {
    /// What is actually executed. The entry point for a binary, and `node`,
    /// `dotnet` or `pwsh` for everything else.
    pub program: PathBuf,
    /// Everything after the program, entry point included where there is one.
    pub args: Vec<String>,
    /// The server itself, which is not always the program.
    pub entry: PathBuf,
    pub version: String,
    pub source: Source,
    pub language: &'static Language,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Override,
    Path,
    VsCodeExtension,
    /// The absolute path a `lsp.servers` entry named. Its own variant rather
    /// than [`Self::Path`] because the banner is what attributes an answer: a
    /// server that was never looked up on `PATH` must not be reported as having
    /// been found there.
    Declared,
}

impl Source {
    fn label(self) -> &'static str {
        match self {
            Self::Override => "the override",
            Self::Path => "PATH",
            Self::VsCodeExtension => "a VS Code extension",
            Self::Declared => "the command in settings.json",
        }
    }
}

/// What [`Identity::LauncherOnly`] puts in the version field. Printed rather
/// than hidden: "Emma did not ask this one what it is" is a fact about the
/// answer's provenance.
pub const UNPROBED: &str = "version not probed";

impl Server {
    /// The one line prepended to every result, for exactly the reason
    /// `Bash::banner` exists: the *description* cannot name the local server,
    /// because the description is hashed into `tool_schema_hash` and a hash that
    /// varies by machine stops being an attribution. So the machine-specific
    /// fact arrives in the result, where it is exact and costs one line.
    ///
    /// The language is in it now, and that is not decoration: with seven
    /// languages in the table, "which server answered" and "which language did
    /// it think this file was" are two different questions, and the Ansible
    /// heuristic makes the second one genuinely uncertain.
    pub fn banner(&self) -> String {
        format!(
            "server: {} ({}) at {} (found via {})",
            self.version,
            self.language.label,
            self.entry.display(),
            self.source.label()
        )
    }
}

impl std::fmt::Display for Server {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} at {}", self.version, self.entry.display())
    }
}

// endregion: The contract

// region: The search
// ---------------------------------------------------------------------------
// The search
//
// Everything the decision depends on is passed in, so the order and every
// refusal are a pure function of a candidate list and a probe: testable on a box
// with no language server at all, and testable for the rustup-proxy case
// without needing a broken rustup to hand.
// ---------------------------------------------------------------------------

/// What a probe learned about one candidate.
pub enum Probe {
    /// It ran and printed something. The string is the first line of stdout.
    Line(String),
    /// It could not be run, or printed nothing useful. The string is carried
    /// into the refusal, which is how the rustup proxy explains itself to the
    /// user rather than being silently skipped.
    Not(String),
}

/// Everything the decision depends on, passed in rather than read.
pub struct Env<'a> {
    pub windows: bool,
    pub path_dirs: Vec<PathBuf>,
    /// Every editor extension directory, highest version first within a prefix.
    pub extension_dirs: Vec<PathBuf>,
    pub exists: &'a dyn Fn(&Path) -> bool,
    /// Run this entry point with this flag and report the first line of stdout.
    pub probe: &'a dyn Fn(&Path, &str) -> Probe,
    /// Where a launcher writes its scratch files. Only PowerShell Editor
    /// Services needs one, and it needs three.
    pub temp_dir: PathBuf,
}

/// The resolved server for one language, or an honest refusal, against the real
/// environment.
///
/// Public because "which server answered my question" must be answerable
/// without reading this file.
pub fn resolve(language: &'static Language) -> Result<Server, ToolError> {
    let exists = |p: &Path| p.is_file();
    let probe = |p: &Path, arg: &str| probe_version(p, arg);
    let env = Env {
        windows: cfg!(windows),
        path_dirs: std::env::var_os("PATH")
            .map(|p| std::env::split_paths(&p).collect())
            .unwrap_or_default(),
        extension_dirs: extension_dirs(),
        exists: &exists,
        probe: &probe,
        temp_dir: std::env::temp_dir(),
    };
    resolve_with(language, over_for(language).as_deref(), &env)
}

/// The override for a language, reading the per-language variable first and
/// falling back to the original name for rust.
fn over_for(language: &Language) -> Option<String> {
    std::env::var(override_env(language))
        .ok()
        .or_else(|| (language.key == "rust").then(|| std::env::var(OVERRIDE_ENV).ok())?)
}

/// The documented order, in one function.
pub fn resolve_with(
    language: &'static Language,
    over: Option<&str>,
    env: &Env<'_>,
) -> Result<Server, ToolError> {
    if let Some(spec) = over.map(str::trim).filter(|s| !s.is_empty()) {
        return resolve_override(language, spec, env);
    }

    // Rejections are collected rather than dropped so the refusal at the bottom
    // can say "I found this and it answered that", which is the difference
    // between a user fixing their rustup in one minute and reverse-engineering
    // this function.
    let mut rejected: Vec<String> = Vec::new();

    // `PATH` before extensions, for every candidate, because a server the user
    // installed deliberately outranks one that arrived with an editor.
    for candidate in language.candidates {
        // A candidate whose name is a path was not looked up anywhere, and the
        // banner says which.
        let source = if is_path_like(candidate.bin) {
            Source::Declared
        } else {
            Source::Path
        };
        for entry in path_entries(candidate, env) {
            match accept(language, candidate, entry, source, env, &mut rejected) {
                Some(server) => return Ok(server),
                None => continue,
            }
        }
    }
    for candidate in language.candidates {
        for entry in bundled_entries(candidate, env) {
            match accept(
                language,
                candidate,
                entry,
                Source::VsCodeExtension,
                env,
                &mut rejected,
            ) {
                Some(server) => return Ok(server),
                None => continue,
            }
        }
    }

    Err(nothing_found(language, env, &rejected))
}

/// One candidate at one location: prove its identity, find its launcher, build
/// the command line. `None` means rejected, and the reason has been recorded.
fn accept(
    language: &'static Language,
    candidate: &Candidate,
    entry: PathBuf,
    source: Source,
    env: &Env<'_>,
    rejected: &mut Vec<String>,
) -> Option<Server> {
    let version = match candidate.identity {
        Identity::VersionLine { arg, expect } => match (env.probe)(&entry, arg) {
            Probe::Line(line) if expect.matches(&line) => line,
            Probe::Line(line) => {
                rejected.push(format!(
                    "{} : `{arg}` said {line:?} rather than {}",
                    entry.display(),
                    expect.describe()
                ));
                return None;
            }
            Probe::Not(why) => {
                rejected.push(format!("{} : {why}", entry.display()));
                return None;
            }
        },
        Identity::LauncherOnly => UNPROBED.to_string(),
    };

    // The launcher is the other half of "is this runnable". A bicep server with
    // no `dotnet` is not a missing server, and telling the user to install the
    // extension they already have would send them the wrong way.
    let program = match candidate.launcher.program() {
        None => entry.clone(),
        Some(name) => match find_on_path(name, env) {
            Some(p) => p,
            None => {
                rejected.push(format!(
                    "{} needs `{name}` to run it, and no `{name}` is on PATH: {}",
                    entry.display(),
                    candidate.launcher.install()
                ));
                return None;
            }
        },
    };

    Some(Server {
        program,
        args: launch_args(candidate, &entry, env),
        entry,
        version,
        source,
        language,
    })
}

/// The command line after the program.
///
/// PowerShell is the only one that is not "the entry point and the candidate's
/// arguments". Its parameter set was read out of a real
/// `Start-EditorServices.ps1` rather than remembered: `-Stdio` belongs to a
/// parameter set of its own, and `-HostName`, `-HostProfileId`, `-HostVersion`,
/// `-BundledModulesPath`, `-LogPath` and `-SessionDetailsPath` are all declared
/// `ValidateNotNullOrEmpty`. `BundledModulesPath` is the `modules` directory,
/// which is the script's own grandparent.
fn launch_args(candidate: &Candidate, entry: &Path, env: &Env<'_>) -> Vec<String> {
    let mut args: Vec<String> = Vec::new();
    match candidate.launcher {
        Launcher::Direct => {}
        Launcher::Node | Launcher::Dotnet => args.push(entry.display().to_string()),
        Launcher::PowerShell => {
            let modules = entry
                .parent()
                .and_then(Path::parent)
                .map(Path::to_path_buf)
                .unwrap_or_else(|| entry.to_path_buf());
            for a in ["-NoLogo", "-NoProfile", "-NonInteractive", "-File"] {
                args.push(a.into());
            }
            args.push(entry.display().to_string());
            args.push("-HostName".into());
            args.push("Emma".into());
            args.push("-HostProfileId".into());
            args.push("emma".into());
            args.push("-HostVersion".into());
            args.push("1.0.0".into());
            args.push("-BundledModulesPath".into());
            args.push(modules.display().to_string());
            args.push("-LogPath".into());
            args.push(env.temp_dir.join("emma-pses.log").display().to_string());
            args.push("-LogLevel".into());
            args.push("Normal".into());
            args.push("-SessionDetailsPath".into());
            args.push(
                env.temp_dir
                    .join("emma-pses-session.json")
                    .display()
                    .to_string(),
            );
            args.push("-LanguageServiceOnly".into());
            args.push("-Stdio".into());
        }
    }
    args.extend(candidate.args.iter().map(|a| a.to_string()));
    args
}

fn path_entries(candidate: &Candidate, env: &Env<'_>) -> Vec<PathBuf> {
    if candidate.bin.is_empty() {
        return Vec::new();
    }
    // A `bin` that is a path is that path, and no `PATH` directory is joined to
    // it. None of the seven built-in candidates spells one — they are all bare
    // names — but a server declared in `settings.json` may be an absolute path,
    // which is the whole point of allowing one: a server that is installed and
    // deliberately not on `PATH`.
    if is_path_like(candidate.bin) {
        return names_for_path(Path::new(candidate.bin), env)
            .into_iter()
            .filter(|c| (env.exists)(c))
            .collect();
    }
    names_for(candidate.bin, env)
        .into_iter()
        .flat_map(|name| env.path_dirs.iter().map(move |d| d.join(&name)))
        .filter(|c| (env.exists)(c))
        .collect()
}

fn bundled_entries(candidate: &Candidate, env: &Env<'_>) -> Vec<PathBuf> {
    let Some(bundled) = &candidate.bundled else {
        return Vec::new();
    };
    env.extension_dirs
        .iter()
        .filter(|dir| {
            dir.file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with(bundled.prefix))
        })
        .flat_map(|dir| {
            let base = bundled
                .relative
                .split('/')
                .fold(dir.clone(), |p, s| p.join(s));
            // A binary inside an extension is `.exe` on Windows and bare
            // elsewhere, and the table spells the bare name.
            names_for_path(&base, env)
        })
        .filter(|c| (env.exists)(c))
        .collect()
}

fn find_on_path(name: &str, env: &Env<'_>) -> Option<PathBuf> {
    names_for(name, env)
        .into_iter()
        .flat_map(|n| env.path_dirs.iter().map(move |d| d.join(&n)))
        .find(|c| (env.exists)(c))
}

/// Whether a spelling names a location rather than a program to look up.
///
/// One rule, used by the override and by a declared `command`, so the two
/// cannot come to disagree about what `C:\tools\ls.exe` is.
fn is_path_like(spec: &str) -> bool {
    spec.contains(['/', '\\']) || Path::new(spec).is_absolute()
}

fn resolve_override(
    language: &'static Language,
    spec: &str,
    env: &Env<'_>,
) -> Result<Server, ToolError> {
    let var = override_env(language);
    let looks_like_path = is_path_like(spec);
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

    let Some(entry) = found else {
        // Refuse rather than fall back. A tool that silently consults a
        // different server than the one you named produces answers that read
        // exactly like the answers you asked for.
        let where_looked = if looks_like_path {
            "no file at that path".to_string()
        } else {
            format!("looked in: {}", dir_list(env))
        };
        return Err(ToolError::Unavailable(format!(
            "{var} names \"{spec}\", and no such language server was found ({where_looked}). \
             Set {var} to an absolute path, or unset it to take the default."
        )));
    };

    // An override is taken at its word about *what* it is, and it still has to
    // exist. The identity probe is skipped deliberately: the user has named a
    // specific file for a specific language, and re-litigating that would make
    // the override useless for exactly the servers it exists to reach, the ones
    // this table has no entry for the shape of.
    Ok(Server {
        program: entry.clone(),
        args: language
            .candidates
            .first()
            .map(|c| c.args.iter().map(|a| a.to_string()).collect())
            .unwrap_or_default(),
        entry,
        version: UNPROBED.to_string(),
        source: Source::Override,
        language,
    })
}

fn names_for(name: &str, env: &Env<'_>) -> Vec<String> {
    if env.windows {
        vec![format!("{name}.exe"), name.to_string()]
    } else {
        vec![name.to_string()]
    }
}

/// The Windows spelling of a full path, for an entry point inside an extension.
/// A `.js`, a `.dll` and a `.ps1` are already spelled in full and get no second
/// candidate; only an extensionless binary does.
fn names_for_path(base: &Path, env: &Env<'_>) -> Vec<PathBuf> {
    if env.windows && base.extension().is_none() {
        vec![base.with_extension("exe"), base.to_path_buf()]
    } else {
        vec![base.to_path_buf()]
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
/// opt-ins. The rejected list is the part that earns its place: on a machine
/// with a rustup proxy the search finds a `rust-analyzer` and refuses it, and a
/// refusal that said only "not found" would be actively misleading to someone
/// looking straight at a `rust-analyzer` on their `PATH`.
fn nothing_found(language: &Language, env: &Env<'_>, rejected: &[String]) -> ToolError {
    let wanted: Vec<&str> = language
        .candidates
        .iter()
        .filter(|c| !c.bin.is_empty())
        .map(|c| c.bin)
        .collect();
    let wanted = if wanted.is_empty() {
        "no server on PATH, only an editor extension".to_string()
    } else {
        wanted.join(", ")
    };
    let mut msg = format!(
        "no {} language server is available. Emma looked for {wanted} in: {}.",
        language.label,
        dir_list(env)
    );
    if !rejected.is_empty() {
        msg.push_str(&format!(
            " Candidates were found and rejected: {}.",
            rejected.join("; ")
        ));
    }
    msg.push_str(&format!(
        " Install it with {}, or set {} to the absolute path of one. Emma will not answer \
         these questions with text search instead: a reference list produced by grep is a \
         different answer to a different question, and nothing in the result would say so.",
        language.install,
        override_env(language)
    ));
    ToolError::Unavailable(msg)
}

/// The refusal for a file no language in the table claims.
///
/// Separate from [`nothing_found`] because the fix is different: one is "install
/// something", the other is "this is not implemented", and because this is the
/// message that has to be unambiguous about not falling back.
pub fn unsupported_language(shown: &str) -> ToolError {
    // The effective table, so a language declared in `settings.json` is listed
    // among the extensions Emma does serve. A refusal that named only the seven
    // would be telling somebody their own entry does not exist.
    let mut extensions: Vec<&str> = lang::table()
        .iter()
        .flat_map(|l| l.extensions.iter().copied())
        .collect();
    extensions.sort_unstable();
    ToolError::Unavailable(format!(
        "Emma has no language server wired for {shown}. The extensions it does serve are: \
         {}. Use Grep for a text search, knowing that it is a text search: it cannot tell a \
         definition from a mention, and it will match the same name in an unrelated project.",
        extensions.join(", ")
    ))
}

/// The refusal for a language that is real, wired, and switched off.
///
/// It is a different sentence from every other refusal here because the fix is
/// one line in a file rather than an install, and because for three of the
/// seven the reason it is off is a fact the user should get to weigh.
pub fn disabled_language(language: &Language) -> ToolError {
    let why = if language.user_declared {
        // A different sentence from the network one below, because the fact is
        // different: this server is off because Emma will not start a program
        // named in a file without being told to, not because of anything known
        // about what it does.
        format!(
            " {} is declared in your settings.json under `lsp.servers`, and a declared server \
             is never on by default: Emma cannot assert `reaches_network: false` about a \
             program it did not choose.",
            language.label
        )
    } else if language.network {
        format!(
            " It is off by default because {} may reach the network while answering, and \
             these tools declare `reaches_network: false`.",
            language.label
        )
    } else {
        String::new()
    };
    ToolError::Unavailable(format!(
        "{} support is not enabled.{why} Add \"{}\" to `lsp.enabled` in settings.json to \
         turn it on.",
        language.label, language.key
    ))
}

// endregion: The search

// region: Presence, without running anything
// ---------------------------------------------------------------------------
// Presence, without running anything
//
// `resolve` is the truth, and it costs a process: every `Identity::VersionLine`
// candidate is executed with `--version` before it is chosen. That is correct
// for a tool call and wrong for a screen, because the Settings screen paints on
// the input thread and a spawn there is a freeze the user cannot cancel.
//
// So this is the weaker question, answered from `stat` alone: is there an entry
// point on disk, and is its launcher on `PATH`. It can say `Present` where
// `resolve` would refuse, and the rustup proxy is exactly that case, so nothing
// built on this may say a server *works*. It may only say a file is there.
// ---------------------------------------------------------------------------

/// What a filesystem-only look found for one language.
///
/// Never proof that a server starts. No candidate was executed, no version was
/// probed and no handshake was attempted; [`resolve`] is the only thing that
/// knows whether the file on disk is the server it is named after.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Presence {
    /// An entry point exists and, where the candidate needs one, its launcher
    /// is on `PATH`.
    Found { entry: PathBuf, source: Source },
    /// Entry points exist, and every one of them needs a launcher that is not
    /// on `PATH`. Naming the launcher is the whole value: the user has the
    /// bicep extension already, and telling them to install it again would send
    /// them the wrong way.
    NeedsLauncher { entry: PathBuf, needs: &'static str },
    /// Nothing on `PATH`, nothing in an editor extension directory.
    Absent,
}

/// [`Presence`] for one language against the real environment, spawning
/// nothing.
pub fn presence(language: &'static Language) -> Presence {
    let exists = |p: &Path| p.is_file();
    // Never called: presence asks no candidate what it is. Supplying a probe
    // that refuses keeps that a fact of this function rather than a promise.
    let probe = |_: &Path, _: &str| Probe::Not("not probed".to_string());
    let env = Env {
        windows: cfg!(windows),
        path_dirs: std::env::var_os("PATH")
            .map(|p| std::env::split_paths(&p).collect())
            .unwrap_or_default(),
        extension_dirs: extension_dirs(),
        exists: &exists,
        probe: &probe,
        temp_dir: std::env::temp_dir(),
    };
    presence_with(language, over_for(language).as_deref(), &env)
}

/// [`resolve_with`]'s search order, with the identity probe left out.
pub fn presence_with(language: &Language, over: Option<&str>, env: &Env<'_>) -> Presence {
    if let Some(spec) = over.map(str::trim).filter(|s| !s.is_empty()) {
        let p = PathBuf::from(spec);
        let found = if is_path_like(spec) {
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
        return match found {
            Some(entry) => Presence::Found {
                entry,
                source: Source::Override,
            },
            None => Presence::Absent,
        };
    }

    // The first entry whose launcher is missing, kept in case no later
    // candidate is runnable: "install dotnet" beats "install the extension you
    // already have".
    let mut blocked: Option<(PathBuf, &'static str)> = None;
    for (source, entries) in [
        (Source::Path, collect(language, env, path_entries)),
        (
            Source::VsCodeExtension,
            collect(language, env, bundled_entries),
        ),
    ] {
        for (candidate, entry) in entries {
            // The same correction `resolve_with` makes, for the same reason.
            let source = if source == Source::Path && is_path_like(candidate.bin) {
                Source::Declared
            } else {
                source
            };
            match candidate.launcher.program() {
                None => return Presence::Found { entry, source },
                Some(name) if find_on_path(name, env).is_some() => {
                    return Presence::Found { entry, source }
                }
                Some(name) => blocked.get_or_insert((entry, name)),
            };
        }
    }
    match blocked {
        Some((entry, needs)) => Presence::NeedsLauncher { entry, needs },
        None => Presence::Absent,
    }
}

/// The startup disclosure for one server declared in `settings.json`.
///
/// **Why this is said out loud, every run.** All seven tools are
/// `read_only: true`, which means the approval gate lets them through without
/// asking anybody — and from this change on, one of them may spawn a program
/// named in a configuration file. Nobody is prompted for that, so the least
/// Emma can do is say which program, before the first call rather than after
/// it. Home-directory settings only: `lsp.servers` is read from
/// `~/.emma/settings.json` and from nowhere else, so a cloned repository cannot
/// declare one.
///
/// Costs a `stat` and no process: [`presence`] is the no-spawn look, and this
/// line says "may start", never "works".
pub fn declared_line(language: &'static Language, enabled: bool) -> String {
    declared_line_from(language, enabled, presence(language))
}

/// [`declared_line`]'s wording, as a function of what was found.
pub fn declared_line_from(language: &Language, enabled: bool, found: Presence) -> String {
    let command = language
        .candidates
        .first()
        .map(|c| c.bin)
        .unwrap_or_default();
    let extensions: Vec<String> = language
        .extensions
        .iter()
        .map(|e| format!(".{e}"))
        .collect();
    let what = match found {
        Presence::Found { entry, .. } => format!(
            "Emma may start {} for {} files",
            entry.display(),
            extensions.join(", ")
        ),
        Presence::NeedsLauncher { entry, needs } => format!(
            "{} is on disk and needs `{needs}`, which is not on PATH",
            entry.display()
        ),
        Presence::Absent => format!(
            "nothing named `{command}` was found, so a call about {} will be refused rather \
             than answered by something else",
            extensions.join(", ")
        ),
    };
    let switch = if enabled {
        "on"
    } else {
        "off — add it to `lsp.enabled` to turn it on"
    };
    format!(
        "settings.json declares a {} language server (`lsp.servers.{:?}`, {switch}): {what}.",
        language.label, language.key
    )
}

/// Every entry point one locator finds, paired with the candidate it came from,
/// in the table's order.
fn collect<'a>(
    language: &'a Language,
    env: &Env<'_>,
    locate: fn(&Candidate, &Env<'_>) -> Vec<PathBuf>,
) -> Vec<(&'a Candidate, PathBuf)> {
    language
        .candidates
        .iter()
        .flat_map(|c| locate(c, env).into_iter().map(move |e| (c, e)))
        .collect()
}

// endregion: Presence, without running anything

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

/// Runs `<candidate> <arg>` and reports the first line of *stdout*.
///
/// Stdout specifically. The rustup proxy prints its complaint to stderr and
/// exits 0, so neither the exit status nor the merged streams distinguish it;
/// the identifying string on the right stream is what does.
fn probe_version(path: &Path, arg: &str) -> Probe {
    // Deliberately `std` and blocking rather than tokio. This runs once per
    // server start, inside a call that is about to wait seconds for indexing,
    // and a blocking spawn here avoids making every caller of `resolve` async
    // for a ten-millisecond process.
    let output = std::process::Command::new(path)
        .arg(arg)
        .stdin(std::process::Stdio::null())
        .output();
    let output = match output {
        Ok(o) => o,
        Err(e) => return Probe::Not(format!("could not be run: {e}")),
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    let line = stdout.lines().next().unwrap_or("").trim().to_string();
    if !line.is_empty() {
        return Probe::Line(line);
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let said = stderr.lines().next().unwrap_or("").trim();
    Probe::Not(format!(
        "`{arg}` printed nothing to stdout{}",
        if said.is_empty() {
            String::new()
        } else {
            format!(" and said {said:?} on stderr")
        }
    ))
}

/// Every editor extension directory, newest first inside each extension.
///
/// Sorted descending by directory name, which puts `redhat.ansible-26.8.2`
/// ahead of `redhat.ansible-26.6.0`. Not a version parse: extension directories
/// carry a platform suffix on some installs — the Windows box has
/// `rust-lang.rust-analyzer-0.3.3033-win32-x64` — and a lexical sort over the
/// whole name is the thing that cannot get a version wrong because it never
/// claims to know where the version is.
fn extension_dirs() -> Vec<PathBuf> {
    let Some(home) = home_dir() else {
        return Vec::new();
    };
    let mut dirs: Vec<PathBuf> = EXTENSION_ROOTS
        .iter()
        .map(|r| r.split('/').fold(home.clone(), |p, s| p.join(s)))
        .filter_map(|root| std::fs::read_dir(root).ok())
        .flat_map(|entries| entries.flatten())
        .filter(|e| e.path().is_dir())
        .map(|e| e.path())
        .collect();
    dirs.sort_by(|a, b| b.file_name().cmp(&a.file_name()));
    dirs
}

/// `USERPROFILE` first, `HOME` second, and the order is load-bearing on
/// Windows.
///
/// Not a dependency for two environment variables. The fork this file was
/// ported from read `HOME` first, which is wrong under every MSYS-derived shell
/// on Windows — Git Bash exports `HOME` as a POSIX-style path that
/// `std::fs::read_dir` cannot open, so the extension search would find nothing
/// on a machine that has the extensions. `USERPROFILE` is Windows-only and
/// absent on Unix, so preferring it costs nothing there.
fn home_dir() -> Option<PathBuf> {
    home_from(
        std::env::var_os("USERPROFILE").map(PathBuf::from),
        std::env::var_os("HOME").map(PathBuf::from),
    )
}

/// The choice above, as a pure function, so the order is a thing a test can
/// hold rather than a comment.
fn home_from(userprofile: Option<PathBuf>, home: Option<PathBuf>) -> Option<PathBuf> {
    userprofile.or(home).filter(|p| !p.as_os_str().is_empty())
}

/// The probe timeout is declared and not yet enforced: `std`'s `output()` has
/// no deadline, and the alternative is a thread per probe, for a case — a
/// `--version` that hangs forever — that has not been observed. Kept as the
/// documented intent, and referenced here so it is not dead code pretending to
/// be a guarantee.
#[allow(dead_code)]
const _PROBE_TIMEOUT_IS_ASPIRATIONAL: Duration = PROBE_TIMEOUT;

// endregion: Probing, and where the extension keeps its server

// region: The two things this file decides that no injected `Env` reaches
// ---------------------------------------------------------------------------
// The two things this file decides that no injected `Env` reaches
//
// `tests/discovery.rs` drives the whole search against an injected `Env`, which
// is why it runs on a box with nothing installed. Two decisions are made
// *building* that `Env` and are therefore outside it, and both have bitten.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod home_tests {
    use super::*;

    /// `USERPROFILE` wins, and on this class of machine that is the difference
    /// between finding the VS Code extensions and finding nothing.
    ///
    /// Measured on the Windows box, 2026-09-06, inside Git Bash:
    ///
    /// ```text
    /// HOME=/c/Users/a
    /// USERPROFILE=C:\Users\a
    /// ```
    ///
    /// `/c/Users/a/.vscode/extensions` is not a path Windows can open, so
    /// reading `HOME` first makes [`extension_dirs`] return an empty vector on a
    /// machine that has four extensions in it — and the symptom is not an error,
    /// it is "no rust-analyzer is available" on a box that has two.
    #[test]
    fn userprofile_is_preferred_to_home_so_an_msys_shell_cannot_hide_the_extensions() {
        let windows_style = PathBuf::from(r"C:\Users\a");
        let msys_style = PathBuf::from("/c/Users/a");
        assert_eq!(
            home_from(Some(windows_style.clone()), Some(msys_style.clone())),
            Some(windows_style),
            "HOME must not win over USERPROFILE"
        );
        // And `HOME` alone is still used, because on Unix it is the only one.
        assert_eq!(home_from(None, Some(msys_style.clone())), Some(msys_style));
        // An empty value is not a home directory; it would join to a relative
        // `.vscode/extensions` under the working directory.
        assert_eq!(home_from(Some(PathBuf::from("")), None), None);
        assert_eq!(home_from(None, None), None);
    }
}
