//! Acting on a run, rather than only reading about one.
//!
//! `harness_state` answers "what ran"; this module is the other half: pause,
//! resume, cancel, archive and delete, plus launching a new run. It exists
//! because the Harness page advertised those controls against a scheduler
//! that does not exist, and the honest fix was not to delete the controls but
//! to notice that Emma already owns the things they name.
//!
//! **A run is a process, and the process is named in the run's own id.** A
//! session id is `sess-<millis>-<pid>` ([`crate::session::SessionLog::new_id`]),
//! so the pid that wrote a session log is recoverable from the log's name. That
//! is the whole backend: pause is `SIGSTOP`, resume is `SIGCONT`, cancel is
//! `SIGINT`, which the loop already handles cleanly and answers by writing its
//! ending. No queue, no supervisor, no invented state.
//!
//! **The dangerous part is that pids are recycled**, so a stored id can name
//! a process that has nothing to do with Emma, and signalling it would be this
//! program reaching into a stranger's. Every signal therefore goes through
//! [`verify`] first, which refuses unless all of these hold:
//!
//! - the id parses and carries a pid above 1 (never init, never a nonsense 0),
//! - the pid is not this process (the TUI must not stop itself),
//! - the running program at that pid is named `emma`,
//! - the run's recorded `cwd` is the repository the page is open on.
//!
//! The last one is policy rather than safety: a Harness page filtered to one
//! repo must not reach into a run somewhere else on the machine, because the
//! reader cannot see that run and cannot judge what stopping it costs.
//!
//! **The seam.** Nothing here calls `kill` directly. [`Control`] is a trait
//! with one real implementation ([`Os`]) and one recording fake in the tests,
//! so the verification logic is tested mechanically and no test ever signals
//! a real process.
//!
//! ## Windows, which is a first-class target and not a `cfg` hole
//!
//! Three of the five things this module does have no Unix-shaped equivalent on
//! Windows, and each answers honestly rather than lying quietly:
//!
//! - **Asking what is running at a pid works.** [`Os::name_of`] is
//!   `OpenProcess` + `QueryFullProcessImageNameW`, so the recycled-pid guard is
//!   as real there as it is on Unix. The image name carries `.exe` and the file
//!   system is case-insensitive, which is why [`is_emma`] exists rather than a
//!   bare `==`.
//! - **Signalling does not.** There is no `SIGSTOP`, `SIGCONT` or `SIGINT` for
//!   an unrelated, detached process. `GenerateConsoleCtrlEvent` reaches only
//!   the caller's own console group, which a detached run is not, and
//!   `TerminateProcess` is *not* what Cancel means — a killed run writes no
//!   ending and leaves a log indistinguishable from a crash. So all three
//!   controls answer [`Refusal::Unsupported`], which names the action and says
//!   why, and [`supported`] lets the page draw words instead of a control that
//!   does nothing.
//! - **Archiving needs a stricter guard.** A Windows rename can succeed over a
//!   handle another process holds, so "the signal would have been refused" is
//!   not enough evidence there. See [`archive`].

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

// region: What a control action is

/// What can be done to a live run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// `SIGSTOP`. The process is frozen where it stands; nothing is written,
    /// including any answer it was midway through.
    Pause,
    /// `SIGCONT`.
    Resume,
    /// `SIGINT`, which the loop treats as the interrupt it already handles.
    /// Not `SIGKILL`: an ended run should write its ending, and a killed one
    /// leaves a log that looks exactly like a crash.
    Cancel,
}

impl Action {
    /// The word written to the session log and shown in the event line.
    pub fn wire(self) -> &'static str {
        match self {
            Self::Pause => "pause",
            Self::Resume => "resume",
            Self::Cancel => "cancel",
        }
    }

    /// The signal name, for the notice. Named rather than numbered because
    /// the number is platform trivia and the name is the thing a reader can
    /// look up.
    ///
    /// It is a Unix name on every platform on purpose: it is only ever shown
    /// beside a signal that was actually sent, and nothing is sent anywhere but
    /// Unix. The Windows refusal does not use it — [`Refusal::Unsupported`]
    /// writes its own sentence, because "SIGSTOP is unavailable" tells a
    /// Windows reader nothing they can act on.
    pub fn signal_name(self) -> &'static str {
        match self {
            Self::Pause => "SIGSTOP",
            Self::Resume => "SIGCONT",
            Self::Cancel => "SIGINT",
        }
    }

    /// The verb, for a sentence that has to read as English.
    fn verb(self) -> &'static str {
        match self {
            Self::Pause => "pausing",
            Self::Resume => "resuming",
            Self::Cancel => "cancelling",
        }
    }

    #[cfg(unix)]
    fn signal(self) -> i32 {
        match self {
            Self::Pause => libc::SIGSTOP,
            Self::Resume => libc::SIGCONT,
            Self::Cancel => libc::SIGINT,
        }
    }
}

/// Whether this platform can carry out `action` on a run at all.
///
/// **The page asks this before it draws.** A control that is present and
/// refuses on every press is the failure this module exists to avoid; a page
/// that knows the answer in advance can print the sentence instead. `false`
/// here means [`control`] will always end in [`Refusal::Unsupported`], and the
/// sentence to print is that refusal's `Display`.
pub const fn supported(action: Action) -> bool {
    // Unix: all three are real signals the loop already handles.
    // Windows: none of the three, and the module header carries the argument.
    // Deliberately not a `match` on `action` — the answer is per platform, not
    // per action, and pretending otherwise would invite a future `Cancel =>
    // true` implemented with `TerminateProcess`.
    let _ = action;
    cfg!(unix)
}

/// Why a control action was refused. Every variant is a sentence the page can
/// show, because a control that goes quiet is the failure this page exists to
/// avoid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// The id is not `sess-<millis>-<pid>`, so no pid can be read out of it.
    NoPid(String),
    /// The pid names this very process.
    Myself(u32),
    /// Nothing is running at that pid.
    Gone(u32),
    /// Something is running at that pid and it is not Emma. The recycled-pid
    /// case, and the only one that would have been a real accident.
    NotEmma { pid: u32, name: String },
    /// The run belongs to a different working directory than the open page.
    Elsewhere { want: String, got: String },
    /// The run already finished, so there is no process to signal.
    Finished,
    /// The platform has no such control.
    ///
    /// **It carries the action** because the three refusals are three different
    /// facts on Windows and one sentence covering all of them would be vague
    /// where the reader needs specifics: two are missing capabilities, and the
    /// third is a capability that exists and is deliberately not used.
    Unsupported(Action),
    /// The signal call itself failed.
    Failed(String),
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoPid(id) => write!(
                f,
                "{id} carries no process id, so there is nothing to signal"
            ),
            Self::Myself(pid) => {
                write!(
                    f,
                    "pid {pid} is this terminal's own process; refusing to signal it"
                )
            }
            Self::Gone(pid) => write!(f, "pid {pid} is no longer running"),
            Self::NotEmma { pid, name } => write!(
                f,
                "pid {pid} is now `{name}`, not emma: the id was reused and this run is gone"
            ),
            Self::Elsewhere { want, got } => {
                write!(
                    f,
                    "that run ran in {got}, not {want}: refusing to signal it"
                )
            }
            Self::Finished => write!(f, "that run already finished; there is no process left"),
            // Two sentences, not one, and the split is the point. Pause and
            // Resume name a capability Windows does not have. Cancel names one
            // it does have and this program will not use, which a reader is
            // entitled to know so they do not go looking for the option.
            Self::Unsupported(Action::Cancel) => write!(
                f,
                "cancelling a run means SIGINT, so the run writes its own ending; Windows cannot \
                 send that to a detached process, and TerminateProcess would kill it mid-write \
                 and leave a log indistinguishable from a crash, so it is not offered here"
            ),
            Self::Unsupported(action) => write!(
                f,
                "{} a run is {} on unix, and Windows has no supported way to suspend another \
                 process, so this control is not offered here",
                action.verb(),
                action.signal_name(),
            ),
            Self::Failed(why) => write!(f, "the signal failed: {why}"),
        }
    }
}

// endregion: What a control action is

// region: The seam

/// The two operating-system questions this module asks, behind a trait so a
/// test can answer them without a process.
pub trait Control {
    /// This process's own pid.
    fn me(&self) -> u32;
    /// The program name running at `pid`, or `None` when nothing is.
    ///
    /// The *leaf* name as the platform reports it, which on Windows includes
    /// the `.exe`. Compare it with [`is_emma`] rather than `==`.
    fn name_of(&self, pid: u32) -> Option<String>;
    /// Send the signal. Only ever called after [`verify`] passed.
    fn send(&self, pid: u32, action: Action) -> Result<(), Refusal>;
}

/// The real one.
pub struct Os;

impl Control for Os {
    fn me(&self) -> u32 {
        std::process::id()
    }

    #[cfg(unix)]
    fn name_of(&self, pid: u32) -> Option<String> {
        // `/proc` where it exists (Linux), `ps` where it does not (macOS).
        // Reading the file is preferred because it cannot be confused by a
        // process whose arguments contain a newline.
        if let Ok(comm) = std::fs::read_to_string(format!("/proc/{pid}/comm")) {
            return Some(comm.trim().to_string());
        }
        let out = std::process::Command::new("ps")
            .args(["-o", "comm=", "-p", &pid.to_string()])
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let line = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if line.is_empty() {
            return None;
        }
        // `ps` reports the path it was started with; the name is the leaf.
        Some(
            Path::new(&line)
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or(line),
        )
    }

    /// `OpenProcess` + `QueryFullProcessImageNameW`, and no `tasklist`.
    ///
    /// **The recycled-pid guard is the whole reason this module is safe, so it
    /// cannot be the thing that is missing on a platform.** The fork returned
    /// `None` here, which made every Windows `verify` answer
    /// [`Refusal::Gone`] — honest for the three signals, which are refused
    /// anyway, and *wrong* for [`archive`], which read that refusal as
    /// "nothing is writing this file".
    ///
    /// `PROCESS_QUERY_LIMITED_INFORMATION` is the narrowest right that answers
    /// the question, and it is the one that works across integrity levels: a
    /// run started from an elevated shell can still be named from an ordinary
    /// one. `tasklist` would need no new API surface and was rejected — it is a
    /// process spawn per check, and this is called on every page refresh.
    #[cfg(windows)]
    fn name_of(&self, pid: u32) -> Option<String> {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::Threading::{
            OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
            PROCESS_QUERY_LIMITED_INFORMATION,
        };

        // SAFETY: `OpenProcess` takes no pointers. A dead or protected pid
        // comes back as a null handle, which is checked before use; the handle
        // is closed on every path out.
        let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if handle.is_null() {
            return None;
        }
        let mut buf = [0u16; 512];
        let mut len = buf.len() as u32;
        // SAFETY: `handle` is live and owned here; `buf` and `len` are a
        // matched buffer and capacity, and `len` is read back only on success.
        let ok = unsafe {
            QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, buf.as_mut_ptr(), &mut len)
        };
        // SAFETY: the handle came from `OpenProcess` above and is closed once.
        unsafe { CloseHandle(handle) };
        if ok == 0 {
            return None;
        }
        let full = String::from_utf16_lossy(&buf[..len as usize]);
        // The full image path; the name is the leaf, as on Unix.
        Some(
            Path::new(&full)
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or(full),
        )
    }

    #[cfg(unix)]
    fn send(&self, pid: u32, action: Action) -> Result<(), Refusal> {
        // SAFETY: `kill` with a verified positive pid and a constant signal.
        // The pid was checked to be a live emma process immediately above the
        // call site; that check is the safety argument, not this line.
        let rc = unsafe { libc::kill(pid as libc::pid_t, action.signal()) };
        if rc == 0 {
            Ok(())
        } else {
            Err(Refusal::Failed(std::io::Error::last_os_error().to_string()))
        }
    }

    /// Refused, in words, on every one of the three.
    ///
    /// Not a `cfg` hole with a cheerful `Ok(())`: the caller shows the sentence
    /// this returns, and the module header carries the argument for why
    /// `TerminateProcess` is not quietly substituted for Cancel.
    #[cfg(windows)]
    fn send(&self, _pid: u32, action: Action) -> Result<(), Refusal> {
        Err(Refusal::Unsupported(action))
    }
}

// endregion: The seam

// region: Verification

/// The program name a run's process must have. Matched on the leaf name only:
/// the binary can live anywhere, but it has to be this one.
pub const BINARY: &str = "emma";

/// Whether a leaf program name from [`Control::name_of`] is this binary.
///
/// **Not `name == BINARY`, and the difference is a platform fact rather than a
/// tolerance.** On Windows the image name is `emma.exe`, and the file system
/// that produced it is case-insensitive, so `EMMA.EXE` names the same file. On
/// Unix `EMMA` is a different program and matching it would be the module
/// reaching into something it was not asked about — so the Unix arm is exact.
///
/// One input shape, two answers, and the answers are deliberate: this is the
/// tolerance being added to exactly the reader that needs it, not to all of
/// them.
pub fn is_emma(name: &str) -> bool {
    #[cfg(windows)]
    {
        let stem = Path::new(name)
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| name.to_string());
        stem.eq_ignore_ascii_case(BINARY)
    }
    #[cfg(not(windows))]
    {
        name == BINARY
    }
}

/// The pid inside a session id, or `None` when the id is not one of ours.
///
/// The shape is `sess-<13 digits>-<pid>`. Only the trailing field is read, and
/// it must parse as a whole number: a suffix like `sess-…-1234x` is not a pid
/// and pretending it is would be guessing at which process to signal.
pub fn pid_of(session_id: &str) -> Option<u32> {
    let rest = session_id.strip_prefix("sess-")?;
    let (millis, pid) = rest.rsplit_once('-')?;
    if millis.is_empty() || !millis.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let pid: u32 = pid.parse().ok()?;
    // 0 is "every process in my group" and 1 is init. Neither is a run.
    if pid <= 1 {
        return None;
    }
    Some(pid)
}

/// What a caller must know about a run before it can be signalled. Built by
/// the shell from a [`crate::harness_state::RunRow`] rather than read here, so
/// this module never has to agree with that one about how a log is parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    /// The session file's id, which is where the pid comes from.
    pub session: String,
    /// The run's recorded working directory, if the log carried one.
    pub cwd: Option<String>,
    /// Whether the run already wrote an ending.
    pub finished: bool,
}

impl Target {
    /// The target for one row of the Harness feed.
    ///
    /// Here rather than in the page so the three fields are read out of a
    /// [`crate::harness_state::RunRow`] in one place: a page that built this by
    /// hand could pass the row's `id` (which carries a `#n` suffix for a goal)
    /// where the `session` belongs, and the pid would come out of the wrong
    /// string.
    pub fn of(row: &crate::harness_state::RunRow) -> Self {
        Self {
            session: row.session.clone(),
            cwd: row.cwd.clone(),
            finished: row.ending.is_some(),
        }
    }
}

/// Decide whether `target` may be signalled from a page open on `cwd`, and
/// return the pid when it may.
///
/// Every refusal below is a real way this could reach the wrong process. The
/// order is deliberate: the cheap textual checks come before the one that
/// costs a `ps` or an `OpenProcess`.
pub fn verify(c: &dyn Control, target: &Target, cwd: &str) -> Result<u32, Refusal> {
    if target.finished {
        return Err(Refusal::Finished);
    }
    match target.cwd.as_deref() {
        Some(dir) if dir == cwd => {}
        Some(dir) => {
            return Err(Refusal::Elsewhere {
                want: cwd.to_string(),
                got: dir.to_string(),
            })
        }
        // A run with no recorded cwd cannot be shown to be this repository's.
        // Refusing is the conservative direction, and the page only lists runs
        // whose cwd matched anyway, so this is a guard rather than a limit.
        None => {
            return Err(Refusal::Elsewhere {
                want: cwd.to_string(),
                got: "an unrecorded directory".to_string(),
            })
        }
    }
    let Some(pid) = pid_of(&target.session) else {
        return Err(Refusal::NoPid(target.session.clone()));
    };
    if pid == c.me() {
        return Err(Refusal::Myself(pid));
    }
    match c.name_of(pid) {
        None => Err(Refusal::Gone(pid)),
        Some(name) if is_emma(&name) => Ok(pid),
        Some(name) => Err(Refusal::NotEmma { pid, name }),
    }
}

/// Verify, then signal. The only way this module sends anything.
pub fn control(
    c: &dyn Control,
    target: &Target,
    cwd: &str,
    action: Action,
) -> Result<u32, Refusal> {
    let pid = verify(c, target, cwd)?;
    c.send(pid, action)?;
    Ok(pid)
}

// endregion: Verification

// region: Recording it

/// Append a record saying a person did this, so the page can show it and the
/// transcript keeps the whole story.
///
/// It is written to the run's own session file, in the same one-JSON-per-line
/// shape everything else in that file uses, by appending rather than
/// rewriting: a live process has the file open for append too, and a rename
/// underneath it would leave that process writing to an unlinked inode.
///
/// The timestamp is [`crate::session::now_ms`], the same clock
/// `SessionLog::append` stamps every other record with, so the reader that
/// folds this file sorts them together.
pub fn record(dir: &Path, session: &str, action: Action, pid: u32) -> Result<()> {
    use std::io::Write;

    let path = session_file(dir, session)?;
    let line = serde_json::json!({
        "kind": "harness_control",
        "at_ms": crate::session::now_ms(),
        "action": action.wire(),
        "signal": action.signal_name(),
        "pid": pid,
    });
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("opening {}", path.display()))?;
    // `\n`, never `\r\n`, on every platform: the session log is LF-delimited
    // JSON and `SessionLog::append` writes it that way. A CRLF here would put a
    // stray `\r` inside the last field of every record this function writes.
    writeln!(file, "{line}").with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

// endregion: Recording it

// region: Archive and delete

/// Where an archived session goes. A subdirectory rather than a marker record
/// because `harness_state::runs` reads one directory level and no deeper, so
/// moving a file here removes it from every feed without a single line of
/// reader code learning a new rule.
pub const ARCHIVE_DIR: &str = "archive";

/// One session's file inside `dir`, refusing any name that could name a file
/// somewhere else.
///
/// The ids these functions receive come from file names this program itself
/// enumerated, so in practice they are already plain. The check is here anyway
/// because two of the three callers are destructive, and "the input is always
/// clean" is the assumption every path-traversal defect was written under. A
/// name with a separator or a `..` in it is refused rather than sanitised:
/// there is no correct file for it to mean.
fn session_file(dir: &Path, session: &str) -> Result<PathBuf> {
    let bad = session.is_empty()
        || session == ".."
        || session.contains('/')
        || session.contains('\\')
        || session.contains('\0');
    if bad {
        anyhow::bail!("`{session}` is not a session id");
    }
    Ok(dir.join(format!("{session}.jsonl")))
}

/// Positive evidence that no process is still writing this run's file.
///
/// **Windows only, and it is not belt-and-braces.** On Unix [`archive`] treats
/// "the signal would have been refused" as proof enough, because every way
/// [`verify`] refuses is a run that cannot be signalled. That reasoning has a
/// hole — `Elsewhere` refuses a run that may be very much alive — and on Unix
/// the hole is narrow: the page only offers archive on rows it listed, and it
/// lists rows whose `cwd` matched. On Windows the same hole is wider and the
/// consequence worse, because Rust's `File` opens with `FILE_SHARE_DELETE`, so
/// the rename *succeeds* over a live writer's handle and the transcript splits
/// in two with no error anywhere.
///
/// So Windows asks the other question. Not "would we have refused to signal
/// it", but "can we show nothing is running there": an ending was written, or
/// the pid names nothing, or it names something that is not Emma. Everything
/// else — including a run in another directory, and an id with no pid in it —
/// is unknown, and unknown does not archive.
#[cfg(windows)]
fn shown_to_have_stopped(c: &dyn Control, target: &Target) -> bool {
    if target.finished {
        return true;
    }
    let Some(pid) = pid_of(&target.session) else {
        // No pid to ask about is not evidence of anything.
        return false;
    };
    if pid == c.me() {
        return false;
    }
    match c.name_of(pid) {
        None => true,
        Some(name) => !is_emma(&name),
    }
}

/// Move one session file into [`ARCHIVE_DIR`].
///
/// Reversible on purpose: this is the control a reader reaches for to tidy the
/// list, and tidying must not be able to lose a transcript. A file already
/// archived is an error naming that, not a silent success.
///
/// **A live run is refused**, which is why this takes a [`Control`] and a
/// [`Target`] rather than a session id. The argument is in the body, and the
/// Windows half of it is in [`shown_to_have_stopped`].
pub fn archive(c: &dyn Control, dir: &Path, target: &Target, cwd: &str) -> Result<PathBuf> {
    // A live run holds this file open for append. Renaming it out from under
    // that process does not stop it writing: the handle follows the inode, so
    // every record after the rename lands in a file with no name, and the
    // archived copy is missing exactly the tail that was still being written.
    // The transcript is split in two and one half is unreachable. `record` on
    // this same module appends rather than rewrites for this reason and says
    // so; archiving is the operation that could not take that way out, so it
    // refuses instead.
    //
    // Live is decided by `verify`, the same pid check pause, resume and cancel
    // use, so there is one answer to "is this run running" rather than two that
    // can disagree. Every way `verify` refuses is a run that cannot be
    // signalled, which is a run that is not live, which is a run that is safe
    // to move: `Refusal::Finished` most of all, since that is the ordinary
    // case. So only the `Ok` arm stops the archive.
    if let Ok(pid) = verify(c, target, cwd) {
        anyhow::bail!(
            "session {} is still running as pid {pid}. Archiving it now would rename the file \
             out from under a process that has it open for append, and the records it writes \
             after that would go nowhere the archived copy can see. Cancel the run first, or \
             wait for it to write its ending.",
            target.session
        );
    }
    // The stricter Windows question, which the paragraph above cannot ask
    // because a `verify` refusal is not the same as a stopped process.
    #[cfg(windows)]
    if !shown_to_have_stopped(c, target) {
        anyhow::bail!(
            "session {} cannot be shown to have stopped, and on Windows a rename succeeds over a \
             handle the running process still holds — the records written after it would go into \
             the moved file where no feed can see them. Wait for the run to write its ending.",
            target.session
        );
    }
    let from = session_file(dir, &target.session)?;
    if !from.is_file() {
        anyhow::bail!("{} is not a session file", from.display());
    }
    let session = target.session.as_str();
    let into = dir.join(ARCHIVE_DIR);
    std::fs::create_dir_all(&into).with_context(|| format!("creating {}", into.display()))?;
    let to = into.join(format!("{session}.jsonl"));
    if to.exists() {
        anyhow::bail!("{} is already archived", session);
    }
    std::fs::rename(&from, &to)
        .with_context(|| format!("moving {} to {}", from.display(), to.display()))?;
    Ok(to)
}

/// Delete one session file outright.
///
/// The two-step confirmation that guards this lives in the page, next to the
/// key that fires it, because the page is where a reader can see what is about
/// to go. This function is the irreversible half and does no asking.
pub fn delete(dir: &Path, session: &str) -> Result<()> {
    let path = session_file(dir, session)?;
    if !path.is_file() {
        anyhow::bail!("{} is not a session file", path.display());
    }
    std::fs::remove_file(&path).with_context(|| format!("removing {}", path.display()))
}

// endregion: Archive and delete

// region: Launching a run

/// What launching a run would execute. Built purely so it can be asserted on
/// without starting anything: `spawn` runs exactly this and nothing else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Launch {
    /// The program to run. An absolute path in practice —
    /// `std::env::current_exe()` — because a bare name would be resolved
    /// against the child's `PATH`, and on Windows `std::process::Command`
    /// resolves `.exe` and nothing else. `usertools.rs` records what that costs
    /// when the program turns out to be a `.cmd`; Emma itself never is.
    pub program: PathBuf,
    pub args: Vec<String>,
    pub cwd: PathBuf,
}

/// The command that runs `goal` headlessly in `cwd`.
///
/// `-p` is the print (non-interactive) mode: the child does the work, writes
/// its own session log, and exits. That log is what makes the child appear on
/// this page as a run, which is the entire integration. There is no handle
/// kept and no supervision: the file system is the channel.
pub fn launch_for(program: PathBuf, goal: &str, cwd: &Path) -> Launch {
    Launch {
        program,
        args: vec!["-p".to_string(), goal.to_string()],
        cwd: cwd.to_path_buf(),
    }
}

/// Start a launch, fully detached, and return the child's pid.
///
/// **Detached is the requirement, not a nicety.** The TUI is going to be
/// closed while the run is still going, and a child in this terminal's process
/// group would take the same `SIGHUP` and die with it. On Unix `setsid` puts
/// the child in its own session so it survives, and the three standard streams
/// go to `/dev/null` so nothing it prints lands on top of the drawn page.
///
/// **On Windows the same requirement has different words.** A console
/// application inherits its parent's console, and a Ctrl-C or a window close on
/// Emma's console would be delivered to the child too; worse, anything the
/// child wrote would land on Emma's own screen. `DETACHED_PROCESS` gives it no
/// console at all — which is right for a `-p` run, and is the one way this
/// differs from `usertools.rs`, whose whole purpose is a child that *does* own
/// a console (`CREATE_NEW_CONSOLE`, and a raw `CreateProcessW` because
/// `std::process` insists on passing the parent's std handles). Here the null
/// stdio does that job, so `CommandExt::creation_flags` is enough and no raw
/// spawn is needed. `CREATE_NEW_PROCESS_GROUP` completes it: without it the
/// child stays in Emma's group and a console Ctrl-C reaches it anyway.
pub fn spawn(launch: &Launch) -> Result<u32> {
    use std::process::{Command, Stdio};

    let mut cmd = Command::new(&launch.program);
    cmd.args(&launch.args)
        .current_dir(&launch.cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // **`setsid`, not `Command::process_group(0)`, and the difference is
        // the requirement.** `usertools::spawn_detached` uses the safe
        // `process_group(0)` and is right to: a shell window only has to not
        // take a signal aimed at Emma's group. A `-p` run has to outlive the
        // terminal itself, and a new process group keeps the controlling
        // terminal — `setsid` is what drops it. std has no safe spelling for
        // that, which is the whole reason this one call is `unsafe` while the
        // other spawn site is not.
        //
        // SAFETY: `setsid` is async-signal-safe and is the only call made
        // between fork and exec.
        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        use windows_sys::Win32::System::Threading::{CREATE_NEW_PROCESS_GROUP, DETACHED_PROCESS};
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }
    let child = cmd
        .spawn()
        .with_context(|| format!("starting {}", launch.program.display()))?;
    Ok(child.id())
}

// endregion: Launching a run

// region: Next-run tool policy

/// A tool-gate preset, written to the repository's `settings.local.json`.
///
/// **These are next-run policy and nothing else.** A running process read its
/// rules at boot and holds them in memory; nothing outside it can change the
/// gate it is already using. Every notice this page shows for these keys says
/// "next run" for that reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Policy {
    /// Read-only tools pre-approved, everything else left to the gate.
    Safe,
    /// Nothing pre-approved: every gated call asks.
    Manual,
    /// Every tool this build knows about in the deny list.
    DenyAll,
}

impl Policy {
    /// The word on the control.
    pub fn label(self) -> &'static str {
        match self {
            Self::Safe => "Safe",
            Self::Manual => "Manual",
            Self::DenyAll => "Deny All",
        }
    }
}

/// The tools that only read. Pre-approving these is what `Safe` means, and the
/// list is deliberately short: anything that can write a file, run a command,
/// reach the network or start another agent is not on it.
///
/// **`BashOutput` is on it and was not on the fork's**, because the fork's
/// build had no background-command tools. Reading a running command's output
/// declares `read_only: true` and reaches no network, which is the same test
/// every other name here passes; `KillShell`, its other half, declares
/// `read_only: false` and stays out.
///
/// Pinned by `every_read_only_name_really_is_read_only`, one-directional: a
/// name here that the registry says can write or can talk to the network is
/// the defect. The reverse is not — `BrowserClose` qualifies on both bits and
/// is deliberately absent, because `Safe` is a floor, not an inventory.
pub const READ_ONLY_TOOLS: [&str; 10] = [
    "Read",
    "Grep",
    "Glob",
    "BashOutput",
    "TaskGet",
    "TaskList",
    "Hover",
    "GoToDefinition",
    "FindReferences",
    "DocumentSymbols",
];

/// Every tool name this build can register. `Deny All` writes all of them,
/// because Emma refuses a `*` in the tool slot of a rule (see
/// [`crate::permissions`]): "deny everything" has to be spelled out, and a
/// list that silently missed a tool would be a protection somebody believed
/// they had.
///
/// **Kept in step by a test, not by memory.**
/// `every_registered_tool_is_in_the_deny_all_list` builds every tool
/// constructor `main.rs` registers and fails naming any whose name is missing
/// here. That test is the reason this list can be trusted; without it the list
/// was wrong, and quietly.
///
/// **It was wrong on arrival, in both directions.** The macOS fork's list this
/// was ported from carried `Diagnostics`, `NotebookEdit` and `Screenshot`,
/// which this build does not register — harmless, since denying a tool that
/// does not exist denies nothing — and it was missing `BashOutput` and
/// `KillShell`, which this build does register. A `Deny All` written from that
/// list left the two background-execution tools callable on the next run,
/// which is the same class of defect the fork's own doc records for
/// `Screenshot`. The list below is derived from `main.rs`'s registrations, and
/// the test is what keeps it derived.
pub const ALL_TOOLS: [&str; 25] = [
    "Read",
    "Write",
    "Edit",
    "Glob",
    "Grep",
    "Bash",
    "BashOutput",
    "KillShell",
    "TaskCreate",
    "TaskGet",
    "TaskList",
    "TaskUpdate",
    "FindReferences",
    "GoToDefinition",
    "Hover",
    "DocumentSymbols",
    "Skill",
    "WebFetch",
    "WebSearch",
    "BrowserOpen",
    "BrowserRead",
    "BrowserAct",
    "BrowserFill",
    "BrowserClose",
    "Delegate",
];

/// The three lists a preset means, in `permissions` order.
fn lists(policy: Policy) -> (Vec<String>, Vec<String>) {
    let owned = |names: &[&str]| names.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    match policy {
        Policy::Safe => (owned(&READ_ONLY_TOOLS), Vec::new()),
        Policy::Manual => (Vec::new(), Vec::new()),
        Policy::DenyAll => (Vec::new(), owned(&ALL_TOOLS)),
    }
}

/// Write `policy` into `file`, keeping every other key the document holds.
///
/// The merge rule is [`crate::permissions::remember`]'s, for the same reason:
/// this file belongs to more than one program and losing somebody's `hooks`
/// block to a settings write is a defect found weeks later. A file that is not
/// JSON, or not an object, or whose `permissions` is not an object, is not
/// written to at all and the caller is told why.
///
/// Within `permissions`, only entries naming a tool this preset governs are
/// replaced: a hand-written `WebFetch(domain:docs.rs)` grant survives a switch
/// to Manual, because Manual means "pre-approve nothing new", not "delete what
/// the operator wrote". A rule with a specifier is left alone entirely; only
/// bare tool names are this page's to manage.
pub fn write_policy(file: &Path, policy: Policy) -> Result<()> {
    let mut doc: serde_json::Value = match std::fs::read_to_string(file) {
        Ok(raw) if raw.trim().is_empty() => serde_json::json!({}),
        Ok(raw) => serde_json::from_str(&raw).with_context(|| {
            format!(
                "{} is not valid JSON. Nothing was written to it; fix the file by hand",
                file.display()
            )
        })?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => serde_json::json!({}),
        Err(e) => return Err(e).with_context(|| format!("reading {}", file.display())),
    };
    let root = doc.as_object_mut().with_context(|| {
        format!(
            "{} does not hold a JSON object, so it is not a settings file. Nothing was written",
            file.display()
        )
    })?;
    let permissions = root
        .entry("permissions")
        .or_insert_with(|| serde_json::json!({}));
    let permissions = permissions.as_object_mut().with_context(|| {
        format!(
            "{}: `permissions` is not an object. Nothing was written to it",
            file.display()
        )
    })?;

    let (allow, deny) = lists(policy);
    for (key, wanted) in [("allow", allow), ("deny", deny)] {
        let slot = permissions
            .entry(key)
            .or_insert_with(|| serde_json::json!([]));
        let list = slot.as_array_mut().with_context(|| {
            format!(
                "{}: `permissions.{key}` is not an array. Nothing was written to it",
                file.display()
            )
        })?;
        // Keep everything that is not a bare name of a tool we manage.
        list.retain(|v| match v.as_str() {
            Some(text) => !ALL_TOOLS.contains(&text),
            None => true,
        });
        for name in wanted {
            list.push(serde_json::Value::String(name));
        }
    }

    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let body = format!("{}\n", serde_json::to_string_pretty(&doc)?);
    let temp = file.with_extension("json.emma-tmp");
    std::fs::write(&temp, body).with_context(|| format!("writing {}", temp.display()))?;
    std::fs::rename(&temp, file).with_context(|| format!("writing {}", file.display()))?;
    Ok(())
}

// endregion: Next-run tool policy

#[cfg(test)]
mod tests;
