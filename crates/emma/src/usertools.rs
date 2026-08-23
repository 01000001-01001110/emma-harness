//! The sidebar's user tools — things the *person* invokes, not the model.
//!
//! `Shell` opens a shell in the working directory, `Code` opens the configured
//! editor, `FileBrowser` opens the OS file manager on the project,
//! `DataExplorer` opens the same file manager on Emma's own data directory,
//! and `Settings` opens `~/.emma/settings.json` in something that can edit it.
//! `Search` and `Memory` are not launches at all — they run inside Emma, and
//! this module says so instead of pretending to start a program.
//!
//! Three rules carry the whole design, and every one exists because breaking
//! it fails silently:
//!
//! **Nothing spawns attached to Emma's console.** The interactive frame owns
//! the alternate screen, so a child that writes to this console wrecks the
//! display — worse than a dead key, because the user cannot tell what
//! happened. Every spawn goes through [`spawn_detached`], the one place per
//! platform that decides how a child is kept off this console: on Windows a
//! shell gets a console window of its own via a raw `CreateProcessW` that
//! inherits no handles (the Spawning region carries the argument, including
//! the measured failure of every shape `std` can express), and a GUI program
//! gets `CREATE_NO_WINDOW` plus null stdio (a hidden console it may scribble
//! on freely); on unix everything gets null stdio and its own process group,
//! and anything that *needs* a terminal is refused at planning time rather
//! than launched into a wrecked screen. Terminal editors are the
//! sharp case: `nvim` is fine on Windows (new console window) and refused on
//! unix, where v1 has no way to give it a terminal of its own — which is also
//! why the unix `Shell` tool opens the terminal *application* (`open -a
//! Terminal`, or a probed emulator) and lets it pick the user's shell, rather
//! than spawning `$SHELL` with nowhere to put it. `settings.rs` records that
//! decision on the `ToolSettings::shell` field.
//!
//! **A configured value is one program name or one absolute path, never a
//! command line.** Splitting a string on spaces is the first half of running
//! text through a shell, and a path with a space in it is not an argument
//! boundary. A value that does not resolve is an error that names it — never
//! word-split, never silently swapped for a probed default, because a
//! substitution looks identical to success right up until the wrong program
//! opens. `$VISUAL`/`$EDITOR` get one concession: the wider world does put
//! command lines in them, so a value there that does not resolve *as a whole*
//! is skipped and the chain continues — skipped, not split.
//!
//! **Probing must not lie, and it must be cheap.** [`catalogue`] may run on
//! every draw. PATH lookups are cached for the life of the process (PATH is
//! fixed at startup; a program installed mid-session appears after restart,
//! which the cache makes true by construction rather than intermittently).
//! `settings.json` is re-read on every call — one small file read — so the
//! Settings tool's own writes show up on the next frame. An unavailable tool
//! names exactly what was looked for, because "no editor configured and none
//! of code, cursor, subl, zed, nvim is on PATH" is fixable and a dead key is
//! not.
//!
//! On `DataExplorer` vs `FileBrowser`: the owner asked for "open explorer or
//! mac version here". The two tools share one mechanism and differ only in
//! target — the project directory versus Emma's data directory
//! (`tools.data_dir`, default `~/.emma`). That difference is the point: one
//! answers "show me my project", the other "show me what Emma keeps about
//! me". If `data_dir` is set to the project they coincide, by choice.
//!
//! The decision layer ([`plan`]) is pure — it takes a [`Machine`] (env, PATH,
//! settings, OS) and returns a [`Launch`] value or an error string — so every
//! choice in this file is testable on any OS without starting anything. Only
//! [`spawn_detached`] touches the real machine.

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use crate::settings::{self, ToolSettings};

// region: The contract
// ---------------------------------------------------------------------------
// The contract
//
// Shared with the shell (term/app.rs), which renders the catalogue into the
// sidebar's TOOLS section and calls `launch` on a keypress. Fixed by
// agreement; extend additively only.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    Shell,
    Code,
    FileBrowser,
    Search,
    Memory,
    DataExplorer,
    Settings,
}

impl Tool {
    /// True for the tools that run inside Emma rather than launching a
    /// program. The shell routes their keys itself; [`launch`] refuses them
    /// with a sentence saying so, because returning `Ok` for a launch that
    /// never happened is a fake receipt.
    pub fn in_app(self) -> bool {
        matches!(self, Tool::Search | Tool::Memory)
    }

    /// Whether the frame *actually* routes this tool's key today.
    ///
    /// **`in_app` was being read as if it meant this, and it does not.** Its
    /// doc says "the shell routes their keys itself", and for `Search` that is
    /// true — `/` opens the command menu. For `Memory` nothing routes anything:
    /// `launch_tool` sends every tool to `launch`, which refuses an in-app tool
    /// with "the frame owns the 'm' key". The frame owns no such key. So the
    /// sidebar advertised `Alt+M`, pressing it produced a warning, and
    /// `tool_rows`' own doc calls that "the exact defect the design's §6
    /// forbids" — a rendered key that does nothing.
    ///
    /// Reported by the owner as "memory ... do not open to their sub TUI
    /// pages", which is exactly right: there is no page. Splitting the two
    /// questions — *does it run inside Emma* and *is it wired up* — is what
    /// lets the sidebar stop claiming otherwise until one is built.
    pub fn routed(self) -> bool {
        matches!(self, Tool::Search | Tool::Memory)
    }

    pub fn label(self) -> &'static str {
        match self {
            Tool::Shell => "Shell",
            Tool::Code => "Code",
            Tool::FileBrowser => "File Browser",
            Tool::Search => "Search",
            Tool::Memory => "Memory",
            Tool::DataExplorer => "Data Explorer",
            Tool::Settings => "Settings",
        }
    }

    pub fn key(self) -> char {
        match self {
            Tool::Shell => 's',
            Tool::Code => 'c',
            Tool::FileBrowser => 'f',
            Tool::Search => '/',
            Tool::Memory => 'm',
            Tool::DataExplorer => 'd',
            Tool::Settings => ',',
        }
    }
}

/// Owner's listing order, which is also the sidebar's display order.
const ALL: [Tool; 7] = [
    Tool::Shell,
    Tool::Code,
    Tool::FileBrowser,
    Tool::Search,
    Tool::Memory,
    Tool::DataExplorer,
    Tool::Settings,
];

#[derive(Debug, Clone)]
pub struct Entry {
    pub tool: Tool,
    pub label: String,
    pub key: char,
    /// What pressing the key will do — "pwsh in C:\src\emma" — or, when
    /// `available` is false, exactly what was looked for and not found.
    pub detail: String,
    pub available: bool,
}

/// Every tool, always all seven, in display order. Unavailable tools stay in
/// the list with `available: false` and a detail naming what is missing: a
/// key that is absent teaches nothing, a key that says "none of code, cursor,
/// subl, zed, nvim is on PATH" is a to-do list.
///
/// Cost: one `settings.json` read plus cached PATH probes — see the module
/// doc. Cheap enough to call on every draw.
pub fn catalogue(cwd: &Path) -> Vec<Entry> {
    catalogue_on(cwd, &RealMachine)
}

/// Launch a tool and say what happened: `Ok("opened pwsh in C:\src\emma")`
/// or an error naming why not. In-app tools are always an `Err` here — the
/// shell handles their keys without coming through this function.
pub fn launch(tool: Tool, cwd: &Path) -> Result<String, String> {
    let m = RealMachine;
    launch_on(tool, cwd, &m, |l| {
        if tool == Tool::Settings {
            // The Settings tool edits a file that may not exist yet. Created
            // through settings.rs — load then save — so the file an editor
            // opens is the file every other reader would have produced, and
            // an existing file is never rewritten (load/save would reformat
            // it, and a settings file that mutates on open is one nobody can
            // diff).
            if let Some(home) = m.home() {
                ensure_settings_file(&home)?;
            }
        }
        adopt(spawn_detached(l)?);
        Ok(())
    })
}

// endregion: The contract

// region: The machine seam
// ---------------------------------------------------------------------------
// The machine seam
//
// Everything the decision layer wants to know about the world, behind a trait
// so a test can be a Mac with no editor or a Windows box with a weird PATH
// without being either. `os()` is data, not cfg, for the same reason: the
// unix refusal rules are asserted by tests that run on Windows.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Os {
    Windows,
    Mac,
    /// Anything unix that is not a Mac. The behavior is defined by what is
    /// probed (`xdg-open`, the terminal-emulator list), not by the kernel.
    Linux,
}

trait Machine {
    fn os(&self) -> Os;
    fn env(&self, name: &str) -> Option<String>;
    /// A PATH lookup: bare name in, full program path out. What "program"
    /// means is per-OS — on Windows the answer may be `code.cmd`, and
    /// resolving that *here* matters because `Command::new("code")` alone
    /// would not find a `.cmd` shim.
    fn find(&self, name: &str) -> Option<PathBuf>;
    fn exists(&self, path: &Path) -> bool;
    fn tools(&self) -> ToolSettings;
    fn home(&self) -> Option<PathBuf>;
}

struct RealMachine;

impl Machine for RealMachine {
    fn os(&self) -> Os {
        if cfg!(windows) {
            Os::Windows
        } else if cfg!(target_os = "macos") {
            Os::Mac
        } else {
            Os::Linux
        }
    }

    fn env(&self, name: &str) -> Option<String> {
        std::env::var(name).ok().filter(|v| !v.trim().is_empty())
    }

    fn find(&self, name: &str) -> Option<PathBuf> {
        // Cached for the life of the process: PATH does not change under a
        // running Emma, and the catalogue may be asked on every draw.
        static CACHE: OnceLock<Mutex<HashMap<String, Option<PathBuf>>>> = OnceLock::new();
        let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
        let mut cache = cache.lock().expect("usertools probe cache poisoned");
        cache
            .entry(name.to_string())
            .or_insert_with(|| std::env::var_os("PATH").and_then(|path| search_in(&path, name)))
            .clone()
    }

    fn exists(&self, path: &Path) -> bool {
        path.exists()
    }

    fn tools(&self) -> ToolSettings {
        self.home()
            .map(|h| settings::load(&h).tools)
            .unwrap_or_default()
    }

    fn home(&self) -> Option<PathBuf> {
        emma_llm::auth::home_dir()
    }
}

/// Walk a PATH value looking for `name`. Split out of [`Machine::find`] and
/// handed the PATH explicitly so tests can probe a constructed directory
/// without mutating the process environment other tests share.
fn search_in(path: &std::ffi::OsStr, name: &str) -> Option<PathBuf> {
    for dir in std::env::split_paths(path) {
        if dir.as_os_str().is_empty() {
            continue;
        }
        for candidate in candidates(name) {
            let p = dir.join(&candidate);
            if is_program(&p) {
                return Some(p);
            }
        }
    }
    None
}

/// The filenames a bare name may resolve to. On Windows a launcher is often a
/// `.cmd` shim (VS Code's `code` is `code.cmd`), and Rust's `Command` does
/// not consult PATHEXT — so the probe must, or `available: true` would name a
/// program the spawn cannot start. The order matches the common PATHEXT
/// prefix: a real `code.exe` beats a `code.cmd` beside it.
#[cfg(windows)]
fn candidates(name: &str) -> Vec<String> {
    let lower = name.to_ascii_lowercase();
    if [".exe", ".cmd", ".bat", ".com"]
        .iter()
        .any(|ext| lower.ends_with(ext))
    {
        return vec![name.to_string()];
    }
    ["exe", "com", "cmd", "bat"]
        .iter()
        .map(|ext| format!("{name}.{ext}"))
        .collect()
}

#[cfg(not(windows))]
fn candidates(name: &str) -> Vec<String> {
    vec![name.to_string()]
}

#[cfg(windows)]
fn is_program(p: &Path) -> bool {
    p.is_file()
}

#[cfg(not(windows))]
fn is_program(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    p.metadata()
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

// endregion: The machine seam

// region: Planning
// ---------------------------------------------------------------------------
// Planning
//
// Pure decisions: tool + machine in, `Launch` value or error string out.
// Nothing in this region starts a process, which is what makes every branch
// below testable on any OS.
// ---------------------------------------------------------------------------

/// How the spawn must be kept off Emma's console. Two shapes, because the two
/// needs are opposite: a shell *wants* a console (a new one), a GUI program
/// must not touch any console we can see.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Window {
    /// A console program the user will interact with: give it a brand-new
    /// console window. Windows-only by construction — unix plans never
    /// produce this, because unix has no OS-made "new terminal window" and
    /// the plans there either use the terminal application or refuse.
    NewConsole,
    /// Everything else: no window of ours, null stdio, free to outlive Emma.
    Detached,
}

/// A fully-decided launch: argv, where, and how to detach. `args` is built
/// element by element — there is deliberately no constructor taking a string.
#[derive(Debug)]
struct Launch {
    program: PathBuf,
    args: Vec<OsString>,
    cwd: PathBuf,
    window: Window,
    /// Present tense, no verb: "pwsh in C:\src\emma". The catalogue
    /// shows it as-is; `launch` prefixes "opened " — one string, two tenses,
    /// so the promise and the receipt cannot drift apart.
    what: String,
}

fn plan(tool: Tool, cwd: &Path, m: &dyn Machine) -> Result<Launch, String> {
    match tool {
        Tool::Shell => plan_shell(cwd, m),
        Tool::Code => plan_code(cwd, m),
        Tool::FileBrowser => plan_file_browser(cwd, m),
        Tool::DataExplorer => plan_data_explorer(cwd, m),
        Tool::Settings => plan_settings(cwd, m),
        Tool::Search | Tool::Memory => Err(in_app_note(tool)),
    }
}

fn plan_shell(cwd: &Path, m: &dyn Machine) -> Result<Launch, String> {
    match m.os() {
        Os::Windows => {
            let program = match m.tools().shell {
                Some(v) => resolve_value(m, &v, "tools.shell")?,
                None => probe_first(m, &["pwsh", "powershell", "cmd"]).ok_or(
                    "none of pwsh, powershell, cmd is on PATH (set tools.shell in settings)",
                )?,
            };
            console_hostable(&program)?;
            Ok(Launch {
                what: format!("{} in {}", stem(&program), cwd.display()),
                program,
                args: vec![],
                cwd: cwd.to_path_buf(),
                window: Window::NewConsole,
            })
        }
        // The terminal app picks the user's login shell itself, which is why
        // v1 does not honor `tools.shell` here — there is nowhere to put a
        // bare shell process without a terminal to hold it. Recorded on the
        // settings field.
        Os::Mac => {
            let program = m
                .find("open")
                .ok_or("the macOS 'open' command is not on PATH")?;
            Ok(Launch {
                what: format!("Terminal in {}", cwd.display()),
                program,
                args: vec![
                    OsString::from("-a"),
                    OsString::from("Terminal"),
                    cwd.as_os_str().to_os_string(),
                ],
                cwd: cwd.to_path_buf(),
                window: Window::Detached,
            })
        }
        // No flags: every emulator on this list starts in its inherited
        // working directory, and per-emulator flag vocabularies are exactly
        // the kind of knowledge that rots.
        Os::Linux => {
            const TERMINALS: [&str; 8] = [
                "x-terminal-emulator",
                "gnome-terminal",
                "konsole",
                "xfce4-terminal",
                "alacritty",
                "kitty",
                "foot",
                "xterm",
            ];
            let program = probe_first(m, &TERMINALS).ok_or_else(|| {
                format!(
                    "no terminal emulator found: looked for {}",
                    TERMINALS.join(", ")
                )
            })?;
            Ok(Launch {
                what: format!("{} in {}", stem(&program), cwd.display()),
                program,
                args: vec![],
                cwd: cwd.to_path_buf(),
                window: Window::Detached,
            })
        }
    }
}

fn plan_code(cwd: &Path, m: &dyn Machine) -> Result<Launch, String> {
    let (editor, source) = resolve_editor(m)?;
    let window = editor_window(&editor, &source, m.os())?;
    if window == Window::NewConsole {
        console_hostable(&editor)?;
    }
    Ok(Launch {
        what: format!("{} in {}", stem(&editor), cwd.display()),
        program: editor,
        args: vec![cwd.as_os_str().to_os_string()],
        cwd: cwd.to_path_buf(),
        window,
    })
}

fn plan_file_browser(cwd: &Path, m: &dyn Machine) -> Result<Launch, String> {
    let program = browser_program(m)?;
    Ok(Launch {
        what: format!("{} at {}", stem(&program), cwd.display()),
        program,
        args: vec![cwd.as_os_str().to_os_string()],
        cwd: cwd.to_path_buf(),
        window: Window::Detached,
    })
}

fn plan_data_explorer(cwd: &Path, m: &dyn Machine) -> Result<Launch, String> {
    let target = match m.tools().data_dir {
        Some(dir) => PathBuf::from(dir),
        None => m
            .home()
            .ok_or("no home directory (HOME/USERPROFILE unset), so ~/.emma has no address")?
            .join(".emma"),
    };
    if !m.exists(&target) {
        return Err(format!(
            "data directory {} does not exist (set tools.data_dir in settings)",
            target.display()
        ));
    }
    let program = browser_program(m)?;
    Ok(Launch {
        what: format!("{} at {}", stem(&program), target.display()),
        program,
        args: vec![target.into_os_string()],
        cwd: cwd.to_path_buf(),
        window: Window::Detached,
    })
}

fn plan_settings(cwd: &Path, m: &dyn Machine) -> Result<Launch, String> {
    let home = m
        .home()
        .ok_or("no home directory (HOME/USERPROFILE unset), so settings.json has no address")?;
    let file = settings::path(&home);
    // The editor first; a plain opener second. The fallback is not a
    // substitution smuggled past the "report, don't swap" rule: the tool's
    // promise is "edit your settings", not "open your chosen editor", and the
    // detail names whichever program will actually appear.
    let (program, args, window) = match resolve_editor(m) {
        Ok((editor, source)) => match editor_window(&editor, &source, m.os()) {
            Ok(Window::NewConsole) if console_hostable(&editor).is_err() => {
                let why = console_hostable(&editor).expect_err("checked");
                fallback_opener(m, &file, &why)?
            }
            Ok(window) => (editor, vec![file.as_os_str().to_os_string()], window),
            Err(why) => fallback_opener(m, &file, &why)?,
        },
        Err(why) => fallback_opener(m, &file, &why)?,
    };
    Ok(Launch {
        what: format!("{} on {}", stem(&program), file.display()),
        program,
        args,
        cwd: cwd.to_path_buf(),
        window,
    })
}

/// Something guaranteed-editable for `settings.json` when the editor chain
/// came up empty (or unusable on this OS). Notepad is on every Windows box;
/// `open -t` is the Mac's "default text editor"; `xdg-open` hands the file to
/// whatever owns `.json`.
fn fallback_opener(
    m: &dyn Machine,
    file: &Path,
    editor_why: &str,
) -> Result<(PathBuf, Vec<OsString>, Window), String> {
    let file_arg = file.as_os_str().to_os_string();
    match m.os() {
        Os::Windows => m
            .find("notepad")
            .map(|p| (p, vec![file_arg], Window::Detached))
            .ok_or_else(|| format!("{editor_why}; and notepad is not on PATH either")),
        Os::Mac => m
            .find("open")
            .map(|p| (p, vec![OsString::from("-t"), file_arg], Window::Detached))
            .ok_or_else(|| format!("{editor_why}; and 'open' is not on PATH either")),
        Os::Linux => m
            .find("xdg-open")
            .map(|p| (p, vec![file_arg], Window::Detached))
            .ok_or_else(|| format!("{editor_why}; and xdg-open is not on PATH either")),
    }
}

/// The file manager: configured value, else the OS's own
/// (`explorer` / `open` / `xdg-open`).
fn browser_program(m: &dyn Machine) -> Result<PathBuf, String> {
    if let Some(v) = m.tools().file_browser {
        return resolve_value(m, &v, "tools.file_browser");
    }
    let name = match m.os() {
        Os::Windows => "explorer",
        Os::Mac => "open",
        Os::Linux => "xdg-open",
    };
    m.find(name)
        .ok_or_else(|| format!("{name} is not on PATH (set tools.file_browser in settings)"))
}

/// The editor chain: `tools.editor`, then `$VISUAL`, then `$EDITOR`, then a
/// probe. Returns the program and where the answer came from, because "vim is
/// a terminal editor" is only actionable if the message can add "from
/// $EDITOR".
fn resolve_editor(m: &dyn Machine) -> Result<(PathBuf, String), String> {
    if let Some(v) = m.tools().editor {
        return resolve_value(m, &v, "tools.editor").map(|p| (p, "tools.editor".to_string()));
    }
    for var in ["VISUAL", "EDITOR"] {
        if let Some(v) = m.env(var) {
            // Resolved as ONE program or skipped whole. The wider world puts
            // command lines in these variables ("vim -u NONE"); word-splitting
            // one is the road to running text through a shell, and failing the
            // whole chain over it would punish a habit Emma did not invent.
            // Skipped, not split — the probe below still finds a usable editor.
            let p = Path::new(&v);
            let resolved = if p.is_absolute() && m.exists(p) {
                Some(p.to_path_buf())
            } else {
                m.find(&v)
            };
            if let Some(p) = resolved {
                return Ok((p, format!("${var}")));
            }
        }
    }
    // nvim is probed only where a new console can hold it. On unix a probe
    // that "found" nvim would immediately be refused two lines later — an
    // unavailability Emma manufactured itself.
    let probe: &[&str] = match m.os() {
        Os::Windows => &["code", "cursor", "subl", "zed", "nvim"],
        Os::Mac | Os::Linux => &["code", "cursor", "subl", "zed"],
    };
    if let Some(p) = probe_first(m, probe) {
        return Ok((p, "PATH".to_string()));
    }
    Err(match m.os() {
        Os::Windows => {
            "no editor configured and none of code, cursor, subl, zed, nvim is on PATH".to_string()
        }
        Os::Mac | Os::Linux => "no editor configured and none of code, cursor, subl, zed is on \
                                PATH (terminal editors such as nvim cannot run while Emma holds \
                                this terminal)"
            .to_string(),
    })
}

/// Decide how an editor may be launched, or refuse. A terminal editor gets a
/// new console on Windows; on unix there is no console to give it, and
/// launching it into Emma's alternate screen would corrupt the display — the
/// refusal names the editor and where it came from so the fix is obvious.
fn editor_window(editor: &Path, source: &str, os: Os) -> Result<Window, String> {
    if !is_terminal_editor(editor) {
        return Ok(Window::Detached);
    }
    match os {
        Os::Windows => Ok(Window::NewConsole),
        Os::Mac | Os::Linux => Err(format!(
            "{} (from {source}) is a terminal editor and Emma is holding this terminal; set \
             tools.editor to a GUI editor",
            stem(editor)
        )),
    }
}

/// Known terminal-only editors, by program stem. A list, not a heuristic: a
/// wrong "GUI" guess corrupts the screen, a wrong "terminal" guess costs one
/// settings line, so unknown names are assumed GUI and the list holds only
/// the certain cases.
fn is_terminal_editor(p: &Path) -> bool {
    let stem = stem(p).to_ascii_lowercase();
    matches!(
        stem.as_str(),
        "nvim" | "vim" | "vi" | "nano" | "hx" | "helix" | "kak" | "micro"
    )
}

/// A new-console launch runs through `CreateProcessW`, which cannot execute a
/// `cmd.exe` script, and routing one through `cmd.exe` would re-open the
/// handle question the raw spawn exists to close. Checked here, at planning
/// time, so the catalogue says so *before* the key is pressed — and mirrored
/// in `new_console::spawn`, which is the layer that must hold even if a plan
/// forgets to ask.
fn console_hostable(program: &Path) -> Result<(), String> {
    let ext = program
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase());
    if matches!(ext.as_deref(), Some("cmd") | Some("bat")) {
        return Err(format!(
            "{} is a cmd script, not a program, and cannot own a console window; point the \
             setting at the underlying .exe",
            program.display()
        ));
    }
    Ok(())
}

/// A configured value from `settings.json`, resolved as one program name or
/// one absolute path — the invariant `ToolSettings`'s own doc promises. A
/// missing value is an error naming it, never a fall-through to a probe: a
/// silent substitute is indistinguishable from success until the wrong
/// program opens.
fn resolve_value(m: &dyn Machine, value: &str, origin: &str) -> Result<PathBuf, String> {
    let p = Path::new(value);
    if p.is_absolute() {
        if m.exists(p) {
            Ok(p.to_path_buf())
        } else {
            Err(format!("{origin} is set to {value}, which does not exist"))
        }
    } else if let Some(found) = m.find(value) {
        Ok(found)
    } else if value.chars().any(char::is_whitespace) {
        Err(format!(
            "{origin} is set to {value}, which is not on PATH (a configured value is one \
             program name or absolute path, never a command line)"
        ))
    } else {
        Err(format!("{origin} is set to {value}, which is not on PATH"))
    }
}

fn probe_first(m: &dyn Machine, names: &[&str]) -> Option<PathBuf> {
    names.iter().find_map(|n| m.find(n))
}

fn stem(p: &Path) -> String {
    p.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.display().to_string())
}

fn in_app_note(tool: Tool) -> String {
    if tool.routed() {
        return format!(
            "{} runs inside Emma, not as a program; the frame owns the '{}' key",
            tool.label(),
            tool.key()
        );
    }
    // **Says what is true, which is that it is not built.** The sentence above
    // was returned for `Memory` too, and it asserted that a key was owned by
    // something that had never claimed it — so the one message a user got when
    // pressing the advertised chord told them the feature worked.
    format!(
        "{} is meant to run inside Emma and has no page yet, so there is nothing to open",
        tool.label()
    )
}

fn in_app_detail(tool: Tool) -> String {
    match tool {
        Tool::Search => "search this project, inside Emma".to_string(),
        Tool::Memory => "what Emma remembers about this project, inside Emma".to_string(),
        // Unreachable by construction; a wrong caller gets words, not a panic.
        other => in_app_note(other),
    }
}

fn catalogue_on(cwd: &Path, m: &dyn Machine) -> Vec<Entry> {
    ALL.iter()
        .map(|&tool| {
            let (detail, available) = if tool.in_app() {
                // Available only when something actually takes the key. An
                // in-app tool with no route is listed, so the operator can see
                // it is intended, and shows `n/a` rather than a chord that
                // warns.
                (in_app_detail(tool), tool.routed())
            } else {
                match plan(tool, cwd, m) {
                    Ok(l) => (l.what, true),
                    Err(e) => (e, false),
                }
            };
            Entry {
                tool,
                label: tool.label().to_string(),
                key: tool.key(),
                detail,
                available,
            }
        })
        .collect()
}

/// The launch decision with the spawner injected — the seam the tests use to
/// prove what would be spawned (and what never is) without starting anything.
fn launch_on(
    tool: Tool,
    cwd: &Path,
    m: &dyn Machine,
    spawner: impl FnOnce(&Launch) -> Result<(), String>,
) -> Result<String, String> {
    if tool.in_app() {
        return Err(in_app_note(tool));
    }
    let l = plan(tool, cwd, m)?;
    spawner(&l)?;
    Ok(format!("opened {}", l.what))
}

/// Create `settings.json` if it does not exist, through `settings.rs` so the
/// file an editor opens is the same file every other writer would produce. An
/// existing file is left byte-for-byte alone.
fn ensure_settings_file(home: &Path) -> Result<(), String> {
    let path = settings::path(home);
    if path.exists() {
        return Ok(());
    }
    settings::save(home, &settings::load(home))
        .map(|_| ())
        .map_err(|e| format!("could not create {}: {e}", path.display()))
}

// endregion: Planning

// region: Spawning
// ---------------------------------------------------------------------------
// Spawning
//
// The only code that starts processes, kept as thin as it can be. The
// detachment argument, per platform:
//
// Windows, `NewConsole`: everything std can express here is wrong, and each
// wrong shape was refuted by measurement, not review. CREATE_NEW_CONSOLE with
// default stdio hands the shell a shiny new window while its actual handles
// still point at Emma's console — Rust sets STARTF_USESTDHANDLES whenever the
// parent has std handles at all (library/std/src/sys/process/windows.rs, and
// the first certify run's canary landed in this process's own output stream).
// Null stdio instead gives an interactive shell a NUL stdout, which it reads
// as redirection and dies (measured: a conhost-hosted client under NUL
// handles exits instantly without running). Hosting under `conhost.exe` flips
// modes on the same axis — real window with no handle info, headless ConPTY
// renderer when handed pipes — so it inherits the same problem. The only
// clean shape is a raw CreateProcessW with bInheritHandles = FALSE and no
// hStd fields: the OS builds the console and binds the child's handles to it,
// and nothing of Emma's can reach the child even in principle. `tools/web`
// spawns its detached Chrome the same way, for the same reason.
//
// Windows, `Detached`: std suffices — CREATE_NO_WINDOW gives the child a
// console with no window (needed because a `.cmd` shim like VS Code's runs
// via cmd.exe, a console program that would otherwise flash or attach), and
// stdio is nulled as well so nothing it prints can reach any console we own.
//
// Unix: null stdio on every spawn — the corruption channel is writes to this
// terminal, and null closes it — plus its own process group so a signal aimed
// at Emma's group is not also aimed at the child. Anything needing a terminal
// was refused at planning time; `Window` is not consulted here because both
// variants detach the same way and `NewConsole` is never planned on unix.
// ---------------------------------------------------------------------------

/// A launched child, held only long enough to let go of it properly.
enum Spawned {
    Std(std::process::Child),
    #[cfg(windows)]
    NewConsole(new_console::Process),
}

#[cfg(windows)]
fn spawn_detached(l: &Launch) -> Result<Spawned, String> {
    use std::os::windows::process::CommandExt;
    use std::process::Stdio;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    match l.window {
        Window::NewConsole => {
            new_console::spawn(&l.program, &l.args, &l.cwd).map(Spawned::NewConsole)
        }
        Window::Detached => {
            let mut cmd = std::process::Command::new(&l.program);
            cmd.args(&l.args)
                .current_dir(&l.cwd)
                .creation_flags(CREATE_NO_WINDOW)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            cmd.spawn()
                .map(Spawned::Std)
                .map_err(|e| format!("{} could not be started: {e}", l.program.display()))
        }
    }
}

#[cfg(not(windows))]
fn spawn_detached(l: &Launch) -> Result<Spawned, String> {
    use std::os::unix::process::CommandExt;
    use std::process::Stdio;
    let mut cmd = std::process::Command::new(&l.program);
    cmd.args(&l.args)
        .current_dir(&l.cwd)
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    cmd.spawn()
        .map(Spawned::Std)
        .map_err(|e| format!("{} could not be started: {e}", l.program.display()))
}

/// Let the child live its own life. Dropping a handle never kills the process;
/// on unix an unreaped child becomes a zombie when it exits before Emma does,
/// so a throwaway thread waits on it — and if Emma exits first the child is
/// init's problem, which is the point.
fn adopt(spawned: Spawned) {
    match spawned {
        #[cfg(windows)]
        Spawned::Std(child) => drop(child),
        #[cfg(windows)]
        Spawned::NewConsole(process) => drop(process),
        #[cfg(not(windows))]
        Spawned::Std(mut child) => {
            std::thread::spawn(move || {
                let _ = child.wait();
            });
        }
    }
}

/// The raw spawn the region comment argues for. Unsafe FFI confined to one
/// small module whose whole surface is `spawn` and a process handle.
#[cfg(windows)]
mod new_console {
    use std::ffi::{OsStr, OsString};
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;

    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
    use windows_sys::Win32::System::Threading::{
        CreateProcessW, GetExitCodeProcess, WaitForSingleObject, CREATE_NEW_CONSOLE, INFINITE,
        PROCESS_INFORMATION, STARTUPINFOW,
    };

    /// An owned process handle. Dropping it closes the handle and leaves the
    /// process running, which is the entire relationship Emma wants with it.
    #[derive(Debug)]
    pub(super) struct Process(HANDLE);

    impl Drop for Process {
        fn drop(&mut self) {
            // Closing a handle never signals the process; failure here has
            // nothing actionable in it.
            unsafe { CloseHandle(self.0) };
        }
    }

    impl Process {
        /// Used only by the ignored certification tests, which need to know
        /// the child ran to completion; the launch path never waits.
        #[allow(dead_code)]
        pub(super) fn wait_exit(&self) -> Result<u32, String> {
            unsafe {
                if WaitForSingleObject(self.0, INFINITE) != WAIT_OBJECT_0 {
                    return Err(format!("wait failed: {}", std::io::Error::last_os_error()));
                }
                let mut code = 0u32;
                if GetExitCodeProcess(self.0, &mut code) == 0 {
                    return Err(format!("exit code: {}", std::io::Error::last_os_error()));
                }
                Ok(code)
            }
        }
    }

    pub(super) fn spawn(program: &Path, args: &[OsString], cwd: &Path) -> Result<Process, String> {
        // The planning layer already refused cmd scripts so the catalogue
        // could say so; re-checked here because this is the layer that must
        // hold even if a future plan forgets to ask.
        super::console_hostable(program)?;

        let mut cmdline: Vec<u16> = Vec::new();
        append_arg(&mut cmdline, program.as_os_str());
        for arg in args {
            cmdline.push(' ' as u16);
            append_arg(&mut cmdline, arg);
        }
        cmdline.push(0);
        // lpApplicationName as well as the command line's first token: the
        // program to run is then never re-derived by parsing the line, spaces
        // or no spaces.
        let app: Vec<u16> = wide(program.as_os_str());
        let dir: Vec<u16> = wide(cwd.as_os_str());

        // SAFETY: every pointer feeds a live, NUL-terminated buffer local to
        // this call; si/pi are zeroed out-params of the documented size.
        // bInheritHandles is FALSE and si.dwFlags stays 0 (no
        // STARTF_USESTDHANDLES), which is the whole argument of the region
        // comment: no handle of this process is reachable from the child.
        unsafe {
            let mut si: STARTUPINFOW = std::mem::zeroed();
            si.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
            let mut pi: PROCESS_INFORMATION = std::mem::zeroed();
            let ok = CreateProcessW(
                app.as_ptr(),
                cmdline.as_mut_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                0, // bInheritHandles = FALSE
                CREATE_NEW_CONSOLE,
                std::ptr::null(),
                dir.as_ptr(),
                &si,
                &mut pi,
            );
            if ok == 0 {
                return Err(format!(
                    "{} could not be started: {}",
                    program.display(),
                    std::io::Error::last_os_error()
                ));
            }
            CloseHandle(pi.hThread);
            Ok(Process(pi.hProcess))
        }
    }

    fn wide(s: &OsStr) -> Vec<u16> {
        s.encode_wide().chain(std::iter::once(0)).collect()
    }

    /// The documented CommandLineToArgvW quoting rules, the same algorithm
    /// std uses for its own spawns: quote when needed, double backslashes
    /// that precede a quote (or the closing quote), escape embedded quotes.
    /// Windows paths cannot contain `"`, but an argument is not always a
    /// path, and a quoting bug here is an argv boundary moving — exactly the
    /// class of failure this module refuses everywhere else.
    fn append_arg(cmdline: &mut Vec<u16>, arg: &OsStr) {
        const QUOTE: u16 = '"' as u16;
        const BACKSLASH: u16 = '\\' as u16;
        let units: Vec<u16> = arg.encode_wide().collect();
        let needs_quotes = units.is_empty()
            || units
                .iter()
                .any(|&u| u == ' ' as u16 || u == '\t' as u16 || u == QUOTE);
        if !needs_quotes {
            cmdline.extend(&units);
            return;
        }
        cmdline.push(QUOTE);
        let mut backslashes = 0usize;
        for &u in &units {
            if u == BACKSLASH {
                backslashes += 1;
            } else {
                if u == QUOTE {
                    // One extra backslash per preceding backslash, plus one
                    // for the quote itself.
                    cmdline.extend(std::iter::repeat_n(BACKSLASH, backslashes + 1));
                }
                backslashes = 0;
            }
            cmdline.push(u);
        }
        // Backslashes before the closing quote would otherwise escape it.
        cmdline.extend(std::iter::repeat_n(BACKSLASH, backslashes));
        cmdline.push(QUOTE);
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn line(program: &str, args: &[&str]) -> String {
            let mut cmdline = Vec::new();
            append_arg(&mut cmdline, OsStr::new(program));
            for a in args {
                cmdline.push(' ' as u16);
                append_arg(&mut cmdline, OsStr::new(a));
            }
            String::from_utf16(&cmdline).unwrap()
        }

        #[test]
        fn quoting_matches_the_commandlinetoargvw_rules() {
            // Plain tokens pass untouched; spaces force quotes; a trailing
            // backslash inside quotes is doubled so it cannot eat the closing
            // quote; embedded quotes are escaped with their preceding
            // backslashes doubled.
            assert_eq!(line("C:\\pwsh\\pwsh.exe", &[]), "C:\\pwsh\\pwsh.exe");
            assert_eq!(
                line("C:\\Program Files\\Odd\\pwsh.exe", &["E:\\a b"]),
                "\"C:\\Program Files\\Odd\\pwsh.exe\" \"E:\\a b\""
            );
            assert_eq!(line("p.exe", &["E:\\dir\\", "x"]), "p.exe E:\\dir\\ x");
            assert_eq!(line("p.exe", &["E:\\a b\\"]), "p.exe \"E:\\a b\\\\\"");
            assert_eq!(line("p.exe", &["say \"hi\""]), "p.exe \"say \\\"hi\\\"\"");
            assert_eq!(line("p.exe", &["a\\\\\"b"]), "p.exe \"a\\\\\\\\\\\"b\"");
            assert_eq!(line("p.exe", &[""]), "p.exe \"\"");
        }

        #[test]
        fn a_cmd_script_is_refused_with_the_fix_named() {
            let e = spawn(Path::new("C:\\shims\\pwsh.cmd"), &[], Path::new("C:\\")).unwrap_err();
            assert!(e.contains("pwsh.cmd"), "{e}");
            assert!(e.contains(".exe"), "{e}");
        }
    }
}

// endregion: Spawning

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::collections::HashSet;

    /// A machine made to order. `find` answers from a map, so a test can be a
    /// Mac with only `open` or a Windows box with three shells, on any host.
    struct Fake {
        os: Os,
        env: Vec<(&'static str, &'static str)>,
        programs: Vec<(&'static str, String)>,
        files: Vec<String>,
        tools: ToolSettings,
        home: Option<String>,
    }

    impl Fake {
        fn new(os: Os) -> Self {
            Fake {
                os,
                env: vec![],
                programs: vec![],
                files: vec![],
                tools: ToolSettings::default(),
                home: Some(match os {
                    Os::Windows => host("C:\\Users\\test"),
                    _ => host("/home/test"),
                }),
            }
        }
    }

    /// Re-spell a fixture path so the **host's** `Path` API parses it the way
    /// the fixture means it.
    ///
    /// The machine under test is fake — `Fake::new(Os::Windows)` models a
    /// Windows box on any host, which is the whole point of `Machine` — but
    /// `plan` resolves the paths it is handed with real `std::path`, and
    /// `std::path` only knows the separator of the platform it was compiled
    /// for. So a literal `C:\pwsh\pwsh.exe` is one filename on unix, whose
    /// `file_stem` is `C:\pwsh\pwsh` and which `is_absolute` denies. That does
    /// not make these tests fail on unix so much as make them assert something
    /// else: `what` reads `C:\pwsh\pwsh in …`, a configured program is looked
    /// for on `PATH` instead of on disk, and `nvim` stops being recognised as a
    /// terminal editor. **The fake OS decides the behaviour; the host decides
    /// the spelling**, and this is the second half.
    ///
    /// A drive letter is dropped rather than translated, because unix has
    /// nothing to translate it to and no assertion here turns on which drive a
    /// path is on — only on two fixture paths differing when they should.
    fn host(path: &str) -> String {
        if cfg!(windows) {
            return path.to_string();
        }
        let bytes = path.as_bytes();
        let rooted = if bytes.len() >= 3 && bytes[1] == b':' && bytes[2] == b'\\' {
            &path[2..]
        } else {
            path
        };
        rooted.replace('\\', "/")
    }

    impl Machine for Fake {
        fn os(&self) -> Os {
            self.os
        }
        fn env(&self, name: &str) -> Option<String> {
            self.env
                .iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| v.to_string())
        }
        fn find(&self, name: &str) -> Option<PathBuf> {
            self.programs
                .iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| PathBuf::from(v))
        }
        fn exists(&self, path: &Path) -> bool {
            let shown: HashSet<PathBuf> = self.files.iter().map(PathBuf::from).collect();
            shown.contains(path)
        }
        fn tools(&self) -> ToolSettings {
            self.tools.clone()
        }
        fn home(&self) -> Option<PathBuf> {
            self.home.as_ref().map(PathBuf::from)
        }
    }

    fn cwd() -> PathBuf {
        PathBuf::from(host("C:\\src\\emma"))
    }

    #[test]
    fn the_catalogue_lists_all_seven_tools_with_their_fixed_keys() {
        // The keys are the shell's contract; a drifted key is a dead key with
        // a working-looking sidebar.
        let m = Fake::new(Os::Windows);
        let entries = catalogue_on(&cwd(), &m);
        let got: Vec<(Tool, char)> = entries.iter().map(|e| (e.tool, e.key)).collect();
        assert_eq!(
            got,
            vec![
                (Tool::Shell, 's'),
                (Tool::Code, 'c'),
                (Tool::FileBrowser, 'f'),
                (Tool::Search, '/'),
                (Tool::Memory, 'm'),
                (Tool::DataExplorer, 'd'),
                (Tool::Settings, ','),
            ]
        );
        assert!(entries.iter().all(|e| !e.label.is_empty()));
        assert!(entries.iter().all(|e| !e.detail.is_empty()));
        // Both in-app tools say where they run, and a probe failure must never
        // mark them dead — that part was always right.
        for t in [Tool::Search, Tool::Memory] {
            let e = entries.iter().find(|e| e.tool == t).unwrap();
            assert!(e.detail.contains("inside Emma"), "{}", e.detail);
        }

        // **This assertion has been all three states, and the record is the
        // point.** It first claimed both in-app tools were available "because
        // their keys are live in the frame" — the claim rather than the
        // behaviour, which held a defect in place: nothing routed Memory, so
        // `Alt+M` produced a warning saying "the frame owns the 'm' key" about a
        // key the frame had never claimed.
        //
        // It was then inverted, with a note saying the day a Memory page existed
        // this line was what would say to flip it back. That day arrived: the
        // frame claims the key now and opens the page. So it is flipped, and the
        // test did exactly the job it was left to do rather than being
        // rediscovered by somebody wondering why the sidebar lied.
        for t in [Tool::Search, Tool::Memory] {
            let e = entries.iter().find(|e| e.tool == t).unwrap();
            assert!(
                e.available,
                "{t:?} is routed by the frame and must offer its key"
            );
        }
    }

    #[test]
    fn windows_shell_probes_pwsh_then_powershell_then_cmd() {
        let mut m = Fake::new(Os::Windows);
        m.programs = vec![
            ("cmd", host("C:\\Windows\\System32\\cmd.exe")),
            ("powershell", host("C:\\ps\\powershell.exe")),
            ("pwsh", host("C:\\pwsh\\pwsh.exe")),
        ];
        let l = plan(Tool::Shell, &cwd(), &m).unwrap();
        assert_eq!(l.program, PathBuf::from(host("C:\\pwsh\\pwsh.exe")));
        // Built from `cwd()` rather than spelled out: the claim is that the
        // stem of the program and the directory it opens in are what `what`
        // carries, and a hand-spelled Windows path turns that into a claim
        // about the host's separator instead.
        assert_eq!(l.what, format!("pwsh in {}", cwd().display()));

        m.programs.retain(|(k, _)| *k != "pwsh");
        let l = plan(Tool::Shell, &cwd(), &m).unwrap();
        assert_eq!(l.program, PathBuf::from(host("C:\\ps\\powershell.exe")));

        m.programs.retain(|(k, _)| *k != "powershell");
        let l = plan(Tool::Shell, &cwd(), &m).unwrap();
        assert_eq!(
            l.program,
            PathBuf::from(host("C:\\Windows\\System32\\cmd.exe"))
        );

        m.programs.clear();
        let e = plan(Tool::Shell, &cwd(), &m).unwrap_err();
        assert!(
            e.contains("pwsh") && e.contains("powershell") && e.contains("cmd"),
            "{e}"
        );
    }

    #[test]
    fn a_configured_program_with_spaces_is_one_path_never_a_command_line() {
        // The invariant settings.rs promises on ToolSettings. If anyone adds
        // word-splitting, the program below stops matching and this goes red.
        let mut m = Fake::new(Os::Windows);
        m.tools.shell = Some(host("C:\\Program Files\\Odd Name\\pwsh.exe"));
        m.files = vec![host("C:\\Program Files\\Odd Name\\pwsh.exe")];
        let l = plan(Tool::Shell, &cwd(), &m).unwrap();
        assert_eq!(
            l.program,
            PathBuf::from(host("C:\\Program Files\\Odd Name\\pwsh.exe"))
        );
        assert!(
            l.args.is_empty(),
            "a split value would show up as args: {:?}",
            l.args
        );
    }

    #[test]
    fn a_configured_tool_that_is_missing_is_reported_not_substituted() {
        // pwsh is right there; the plan must still refuse, because a silent
        // substitute is indistinguishable from success until the wrong
        // program opens.
        let mut m = Fake::new(Os::Windows);
        m.programs = vec![("pwsh", host("C:\\pwsh\\pwsh.exe"))];
        m.tools.shell = Some("fish".into());
        let e = plan(Tool::Shell, &cwd(), &m).unwrap_err();
        assert!(e.contains("fish"), "{e}");
        assert!(e.contains("tools.shell"), "{e}");

        let entry = catalogue_on(&cwd(), &m)
            .into_iter()
            .find(|e| e.tool == Tool::Shell)
            .unwrap();
        assert!(!entry.available);
        assert!(entry.detail.contains("fish"), "{}", entry.detail);

        // A relative value with spaces gets the extra sentence: it is almost
        // certainly a command line, and the rule it breaks should be named.
        m.tools.shell = Some("pwsh -NoLogo".into());
        let e = plan(Tool::Shell, &cwd(), &m).unwrap_err();
        assert!(e.contains("never a command line"), "{e}");
    }

    #[test]
    fn the_editor_chain_is_setting_then_visual_then_editor_then_probe() {
        let mut m = Fake::new(Os::Windows);
        m.programs = vec![
            ("code", host("C:\\vs\\code.cmd")),
            ("cursor", host("C:\\cur\\cursor.exe")),
            ("visual-ed", host("C:\\v\\visual-ed.exe")),
            ("editor-ed", host("C:\\e\\editor-ed.exe")),
            ("set-ed", host("C:\\s\\set-ed.exe")),
        ];
        m.env = vec![("VISUAL", "visual-ed"), ("EDITOR", "editor-ed")];
        m.tools.editor = Some("set-ed".into());

        let (p, src) = resolve_editor(&m).unwrap();
        assert_eq!(
            (p, src.as_str()),
            (PathBuf::from(host("C:\\s\\set-ed.exe")), "tools.editor")
        );

        m.tools.editor = None;
        let (p, src) = resolve_editor(&m).unwrap();
        assert_eq!(
            (p, src.as_str()),
            (PathBuf::from(host("C:\\v\\visual-ed.exe")), "$VISUAL")
        );

        m.env = vec![("EDITOR", "editor-ed")];
        let (p, src) = resolve_editor(&m).unwrap();
        assert_eq!(
            (p, src.as_str()),
            (PathBuf::from(host("C:\\e\\editor-ed.exe")), "$EDITOR")
        );

        // Probe order: code beats cursor when both are present.
        m.env = vec![];
        let (p, src) = resolve_editor(&m).unwrap();
        assert_eq!(
            (p, src.as_str()),
            (PathBuf::from(host("C:\\vs\\code.cmd")), "PATH")
        );
    }

    #[test]
    fn an_editor_env_var_holding_a_command_line_is_skipped_not_split() {
        let mut m = Fake::new(Os::Windows);
        m.env = vec![("EDITOR", "vim -u NONE")];
        m.programs = vec![
            ("vim", host("C:\\vim\\vim.exe")),
            ("code", host("C:\\vs\\code.cmd")),
        ];
        let (p, _) = resolve_editor(&m).unwrap();
        // Split, this would be vim (with args smuggled somewhere); skipped
        // whole, the chain falls through to the probe and finds code.
        assert_eq!(p, PathBuf::from(host("C:\\vs\\code.cmd")));
    }

    #[test]
    fn a_terminal_editor_cannot_open_inside_emmas_terminal() {
        // Same resolution, opposite verdicts: Windows can hand nvim a new
        // console window; unix has nowhere to put it and must say so instead
        // of launching it into the alternate screen.
        let mut m = Fake::new(Os::Linux);
        m.env = vec![("EDITOR", "vim")];
        m.programs = vec![("vim", host("/usr/bin/vim"))];
        let e = plan(Tool::Code, &cwd(), &m).unwrap_err();
        assert!(e.contains("terminal editor"), "{e}");
        assert!(e.contains("$EDITOR"), "{e}");

        let mut m = Fake::new(Os::Windows);
        m.env = vec![("EDITOR", "nvim")];
        m.programs = vec![("nvim", host("C:\\nvim\\nvim.exe"))];
        let l = plan(Tool::Code, &cwd(), &m).unwrap();
        assert_eq!(l.window, Window::NewConsole);
    }

    #[test]
    fn no_editor_anywhere_names_every_candidate_probed() {
        let m = Fake::new(Os::Windows);
        let e = plan(Tool::Code, &cwd(), &m).unwrap_err();
        assert_eq!(
            e,
            "no editor configured and none of code, cursor, subl, zed, nvim is on PATH"
        );

        // The unix message probes a shorter list and must say why.
        let m = Fake::new(Os::Linux);
        let e = plan(Tool::Code, &cwd(), &m).unwrap_err();
        assert!(e.contains("code, cursor, subl, zed"), "{e}");
        assert!(e.contains("nvim"), "{e}");
    }

    #[test]
    fn gui_launches_are_detached_and_shells_get_their_own_console() {
        let mut m = Fake::new(Os::Windows);
        m.programs = vec![
            ("pwsh", host("C:\\pwsh\\pwsh.exe")),
            ("code", host("C:\\vs\\code.cmd")),
            ("explorer", host("C:\\Windows\\explorer.exe")),
        ];
        assert_eq!(
            plan(Tool::Shell, &cwd(), &m).unwrap().window,
            Window::NewConsole
        );
        assert_eq!(
            plan(Tool::Code, &cwd(), &m).unwrap().window,
            Window::Detached
        );
        assert_eq!(
            plan(Tool::FileBrowser, &cwd(), &m).unwrap().window,
            Window::Detached
        );

        // Unix plans never ask for a new console: there is no OS-made one to
        // ask for, and the spawner's unix arm does not consult the field.
        for os in [Os::Mac, Os::Linux] {
            let mut m = Fake::new(os);
            m.programs = vec![
                ("open", host("/usr/bin/open")),
                ("xdg-open", host("/usr/bin/xdg-open")),
                ("gnome-terminal", host("/usr/bin/gnome-terminal")),
                ("code", host("/usr/bin/code")),
            ];
            m.files = vec![host("/home/test/.emma")];
            for tool in [
                Tool::Shell,
                Tool::Code,
                Tool::FileBrowser,
                Tool::DataExplorer,
                Tool::Settings,
            ] {
                let l =
                    plan(tool, &cwd(), &m).unwrap_or_else(|e| panic!("{tool:?} on {os:?}: {e}"));
                assert_eq!(l.window, Window::Detached, "{tool:?} on {os:?}");
            }
        }
    }

    #[test]
    fn the_file_browser_opens_the_project_and_the_data_explorer_opens_the_data_dir() {
        let mut m = Fake::new(Os::Windows);
        m.programs = vec![("explorer", host("C:\\Windows\\explorer.exe"))];
        m.files = vec![host("C:\\Users\\test\\.emma")];

        let fb = plan(Tool::FileBrowser, &cwd(), &m).unwrap();
        assert_eq!(fb.args, vec![OsString::from(host("C:\\src\\emma"))]);

        let de = plan(Tool::DataExplorer, &cwd(), &m).unwrap();
        assert_eq!(
            de.args,
            vec![OsString::from(host("C:\\Users\\test\\.emma"))]
        );
        assert!(de.what.contains(".emma"), "{}", de.what);

        // A configured data_dir wins over the default.
        m.tools.data_dir = Some(host("D:\\emma-data"));
        m.files = vec![host("D:\\emma-data")];
        let de = plan(Tool::DataExplorer, &cwd(), &m).unwrap();
        assert_eq!(de.args, vec![OsString::from(host("D:\\emma-data"))]);
    }

    #[test]
    fn a_cmd_script_cannot_be_promised_a_console_window() {
        // The catalogue must say this at draw time, not at keypress:
        // CreateProcessW cannot execute a cmd script, and routing it through
        // cmd.exe would re-open the handle inheritance question.
        let mut m = Fake::new(Os::Windows);
        m.tools.shell = Some(host("C:\\shims\\pwsh.cmd"));
        m.files = vec![host("C:\\shims\\pwsh.cmd")];
        let e = plan(Tool::Shell, &cwd(), &m).unwrap_err();
        assert!(e.contains("pwsh.cmd"), "{e}");
        assert!(e.contains(".exe"), "{e}");

        // Same rule for a terminal editor bound for a new console; a GUI
        // editor as a .cmd shim (VS Code's own layout) stays fine, because it
        // takes the Detached path where std handles cmd.exe itself.
        let mut m = Fake::new(Os::Windows);
        m.env = vec![("EDITOR", "nvim")];
        m.programs = vec![("nvim", host("C:\\shims\\nvim.cmd"))];
        assert!(plan(Tool::Code, &cwd(), &m).is_err());
        let mut m = Fake::new(Os::Windows);
        m.programs = vec![("code", host("C:\\vs\\code.cmd"))];
        assert!(plan(Tool::Code, &cwd(), &m).is_ok());
    }

    #[test]
    fn a_missing_data_dir_is_named_not_opened() {
        let mut m = Fake::new(Os::Windows);
        m.programs = vec![("explorer", host("C:\\Windows\\explorer.exe"))];
        // home exists but ~/.emma was never created on this machine
        let e = plan(Tool::DataExplorer, &cwd(), &m).unwrap_err();
        assert!(e.contains(".emma"), "{e}");
        assert!(e.contains("does not exist"), "{e}");
    }

    #[test]
    fn search_and_memory_never_reach_the_spawner() {
        // Two layers say no — launch_on's early return and plan's own refusal
        // — and this asserts the observable sum: no spawn, an honest sentence.
        let m = Fake::new(Os::Windows);
        for tool in [Tool::Search, Tool::Memory] {
            let spawned = Cell::new(0u32);
            let r = launch_on(tool, &cwd(), &m, |_| {
                spawned.set(spawned.get() + 1);
                Ok(())
            });
            let e = r.unwrap_err();
            assert!(e.contains("inside Emma"), "{e}");
            assert!(e.contains(&tool.key().to_string()), "{e}");
            assert_eq!(spawned.get(), 0, "{tool:?} spawned something");
        }
    }

    #[test]
    fn an_unavailable_tool_never_reaches_the_spawner() {
        let m = Fake::new(Os::Windows); // nothing installed at all
        let spawned = Cell::new(0u32);
        let r = launch_on(Tool::Shell, &cwd(), &m, |_| {
            spawned.set(spawned.get() + 1);
            Ok(())
        });
        assert!(r.is_err());
        assert_eq!(spawned.get(), 0);
    }

    #[test]
    fn a_failed_spawn_is_an_error_not_a_success() {
        let mut m = Fake::new(Os::Windows);
        m.programs = vec![("pwsh", host("C:\\pwsh\\pwsh.exe"))];
        let r = launch_on(Tool::Shell, &cwd(), &m, |_| Err("boom".to_string()));
        assert_eq!(r.unwrap_err(), "boom");
    }

    #[test]
    fn launch_returns_what_happened_in_words() {
        let mut m = Fake::new(Os::Windows);
        m.programs = vec![("pwsh", host("C:\\pwsh\\pwsh.exe"))];
        let r = launch_on(Tool::Shell, &cwd(), &m, |l| {
            assert_eq!(l.program, PathBuf::from(host("C:\\pwsh\\pwsh.exe")));
            Ok(())
        });
        assert_eq!(r.unwrap(), format!("opened pwsh in {}", cwd().display()));
    }

    #[test]
    fn settings_opens_the_settings_file_with_the_editor_or_a_fallback() {
        let mut m = Fake::new(Os::Windows);
        m.programs = vec![
            ("code", host("C:\\vs\\code.cmd")),
            ("notepad", host("C:\\Windows\\notepad.exe")),
        ];
        let l = plan(Tool::Settings, &cwd(), &m).unwrap();
        assert_eq!(l.program, PathBuf::from(host("C:\\vs\\code.cmd")));
        assert_eq!(
            l.args,
            vec![OsString::from(host(
                "C:\\Users\\test\\.emma\\settings.json"
            ))]
        );

        // No editor: notepad carries it, and the detail says notepad.
        m.programs = vec![("notepad", host("C:\\Windows\\notepad.exe"))];
        let l = plan(Tool::Settings, &cwd(), &m).unwrap();
        assert_eq!(l.program, PathBuf::from(host("C:\\Windows\\notepad.exe")));
        assert!(l.what.starts_with("notepad on "), "{}", l.what);

        // Neither: both absences named, so the user can fix either one.
        m.programs = vec![];
        let e = plan(Tool::Settings, &cwd(), &m).unwrap_err();
        assert!(e.contains("no editor configured"), "{e}");
        assert!(e.contains("notepad"), "{e}");

        // On unix a terminal-only $EDITOR falls back to the opener rather
        // than refusing outright — the file still gets edited, and the detail
        // names the program that will actually appear.
        let mut m = Fake::new(Os::Linux);
        m.env = vec![("EDITOR", "vim")];
        m.programs = vec![
            ("vim", host("/usr/bin/vim")),
            ("xdg-open", host("/usr/bin/xdg-open")),
        ];
        let l = plan(Tool::Settings, &cwd(), &m).unwrap();
        assert_eq!(l.program, PathBuf::from(host("/usr/bin/xdg-open")));
    }

    #[test]
    fn ensure_settings_file_creates_once_and_never_clobbers() {
        let home = tempfile::tempdir().unwrap();
        ensure_settings_file(home.path()).unwrap();
        let path = settings::path(home.path());
        assert!(path.exists());
        // What it wrote is what settings.rs would read back — same producer.
        let _ = settings::load(home.path());

        // A file with a choice in it survives the ensure untouched.
        let mut s = settings::load(home.path());
        s.tools.editor = Some(host("C:\\somewhere\\code.exe"));
        settings::save(home.path(), &s).unwrap();
        let before = std::fs::read_to_string(&path).unwrap();
        ensure_settings_file(home.path()).unwrap();
        let after = std::fs::read_to_string(&path).unwrap();
        assert_eq!(before, after);
    }

    #[test]
    fn mac_shell_opens_terminal_app_and_linux_probes_terminal_emulators() {
        let mut m = Fake::new(Os::Mac);
        m.programs = vec![("open", host("/usr/bin/open"))];
        let l = plan(Tool::Shell, &cwd(), &m).unwrap();
        assert_eq!(l.program, PathBuf::from(host("/usr/bin/open")));
        assert_eq!(
            l.args,
            vec![
                OsString::from("-a"),
                OsString::from("Terminal"),
                OsString::from(host("C:\\src\\emma")),
            ]
        );

        let mut m = Fake::new(Os::Linux);
        m.programs = vec![
            ("xterm", host("/usr/bin/xterm")),
            ("konsole", host("/usr/bin/konsole")),
        ];
        let l = plan(Tool::Shell, &cwd(), &m).unwrap();
        // konsole outranks xterm in the probe order; cwd rides on the spawn's
        // working directory, not on flags.
        assert_eq!(l.program, PathBuf::from(host("/usr/bin/konsole")));
        assert!(l.args.is_empty());
        assert_eq!(l.cwd, cwd());

        m.programs.clear();
        let e = plan(Tool::Shell, &cwd(), &m).unwrap_err();
        assert!(e.contains("gnome-terminal") && e.contains("xterm"), "{e}");
    }

    #[cfg(windows)]
    #[test]
    fn the_path_probe_finds_cmd_shims_the_way_the_spawner_needs_them() {
        // Command::new("code") cannot start a `.cmd` shim; the probe must
        // therefore return the shim's full name, extension and all, or the
        // catalogue would promise a launch the spawn cannot perform.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("code.cmd"), "@echo off\r\n").unwrap();
        let path_var = std::env::join_paths([dir.path()]).unwrap();
        let found = search_in(&path_var, "code").unwrap();
        assert_eq!(found, dir.path().join("code.cmd"));

        // A real .exe beside the shim wins, matching PATHEXT precedence.
        std::fs::write(dir.path().join("code.exe"), "MZ").unwrap();
        let found = search_in(&path_var, "code").unwrap();
        assert_eq!(found, dir.path().join("code.exe"));

        // A name that already carries its extension is taken literally.
        let found = search_in(&path_var, "code.cmd").unwrap();
        assert_eq!(found, dir.path().join("code.cmd"));
    }

    /// The unix counterpart, and it asserts the opposite half of the same
    /// contract: `available: true` must mean the spawn can start it.
    ///
    /// On Windows that means resolving the extension a `.cmd` shim hides
    /// behind; on unix there are no extensions to resolve and the thing that
    /// separates a program from a file is the **executable bit**, which
    /// `is_program` consults and `p.is_file()` — the Windows arm of the same
    /// function — does not. Without this, that `mode & 0o111` had no test on
    /// any platform, and dropping it would have turned every readable file on
    /// `PATH` into an installed editor.
    #[cfg(unix)]
    #[test]
    fn the_path_probe_requires_the_executable_bit() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path_var = std::env::join_paths([dir.path()]).unwrap();
        let file = dir.path().join("code");
        std::fs::write(&file, "#!/bin/sh\n").unwrap();

        // Readable but not executable: not a program, however much it looks
        // like one.
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(
            search_in(&path_var, "code").is_none(),
            "a non-executable file was offered as a program"
        );

        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(search_in(&path_var, "code"), Some(file));

        // And no extension is invented: `code.cmd` is a different name here,
        // not another spelling of this one.
        assert!(search_in(&path_var, "code.cmd").is_none());
    }

    // region: Certification against the real machine
    // -----------------------------------------------------------------------
    // Certification against the real machine
    //
    // Ignored: these spawn real processes (briefly, invisibly — nothing that
    // opens a window that stays). Run by hand with
    //   cargo test -p emma usertools::tests::certify -- --ignored --nocapture
    // and read the OUTPUT, not just the pass: the NewConsole test's claim is
    // that the canary does NOT appear anywhere in the run's output, which the
    // assertion cannot see (an inherited handle bypasses the test harness's
    // capture) — the reader checking the scrollback is the certification.
    // -----------------------------------------------------------------------

    #[cfg(windows)]
    #[test]
    #[ignore = "spawns real processes; run by hand and read the output"]
    fn certify_a_new_console_child_gets_a_real_console_that_is_not_ours() {
        // Three claims, all measured. The hosted shell writes a receipt file
        // naming (a) whether its stdout is a real console — False here means
        // interactive rendering works, and would be True if conhost had passed
        // Emma's null handles through — and (b) its working directory, proving
        // the launch cwd survives the conhost hop. And (c) the canary it also
        // writes to stdout must NOT appear in this test run's output: an
        // inherited handle bypasses the harness's capture, so seeing nothing
        // in the scrollback is the proof the handles are not Emma's. The first
        // version of this spawn — std with bare CREATE_NEW_CONSOLE, default
        // stdio — shipped its canary straight into this process's output;
        // that is the bug this test exists to keep dead.
        let dir = tempfile::tempdir().unwrap();
        let receipt = dir.path().join("receipt.txt");
        let script = format!(
            "Write-Output USERTOOLS-CANARY-1; Set-Content -LiteralPath '{}' \
             -Value ([Console]::IsOutputRedirected.ToString() + '|' + (Get-Location).Path); \
             exit 0",
            receipt.display()
        );
        let l = Launch {
            program: RealMachine.find("powershell").expect("powershell on PATH"),
            args: vec![
                OsString::from("-NoProfile"),
                OsString::from("-Command"),
                OsString::from(script),
            ],
            cwd: dir.path().to_path_buf(),
            window: Window::NewConsole,
            what: String::new(),
        };
        let Spawned::NewConsole(process) = spawn_detached(&l).unwrap() else {
            panic!("a NewConsole launch took the Detached path");
        };
        assert_eq!(process.wait_exit().unwrap(), 0);
        let got = std::fs::read_to_string(&receipt).unwrap();
        let (redirected, cwd) = got.trim().split_once('|').unwrap();
        assert_eq!(
            redirected, "False",
            "hosted shell has no real console: {got}"
        );
        assert_eq!(
            std::fs::canonicalize(cwd).unwrap(),
            std::fs::canonicalize(dir.path()).unwrap()
        );
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "spawns real processes; run by hand and read the output"]
    fn certify_a_detached_child_runs_without_touching_this_console() {
        let l = Launch {
            program: RealMachine.find("powershell").expect("powershell on PATH"),
            args: vec![
                OsString::from("-NoProfile"),
                OsString::from("-Command"),
                OsString::from("Write-Output USERTOOLS-CANARY-2; exit 7"),
            ],
            cwd: std::env::temp_dir(),
            window: Window::Detached,
            what: String::new(),
        };
        let Spawned::Std(mut child) = spawn_detached(&l).unwrap() else {
            panic!("a Detached launch took the NewConsole path");
        };
        // exit 7 coming back proves the child really ran; the canary staying
        // out of this output proves its stdout went to the null device.
        assert_eq!(child.wait().unwrap().code(), Some(7));
    }

    #[test]
    #[ignore = "probes the real machine; run by hand to see what this box has"]
    fn certify_the_catalogue_on_this_machine() {
        let cwd = std::env::current_dir().unwrap();
        for e in catalogue(&cwd) {
            println!(
                "{} [{}] available={} -- {}",
                e.label, e.key, e.available, e.detail
            );
        }
    }

    // endregion: Certification against the real machine
}
