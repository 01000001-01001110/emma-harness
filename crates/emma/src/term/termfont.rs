//! Setting the terminal's own font, from inside the terminal.
//!
//! Emma cannot choose a font. A terminal emulator draws the cells and Emma
//! writes into them, so the honest row for years was "your terminal owns
//! this". What changed is that Terminal.app is scriptable, and the mechanism
//! is one Emma already uses elsewhere: shelling `osascript` to reach another
//! application. The same tool reaches Terminal's own settings.
//!
//! **The gate is data, never `cfg`.** What decides whether a font can be asked
//! for is the terminal the process is *running in*, not the operating system it
//! was *compiled for*: a Windows build inside Windows Terminal, a macOS build
//! inside WezTerm and a Linux build inside anything all reach the same answer
//! by the same route. So [`detect`] takes the environment as arguments and
//! returns a [`Terminal`], and every other function in this module takes that
//! value. There is not one `#[cfg]` in this file, and adding one would be a
//! regression: `#[cfg(not(target_os = "macos"))] fn scriptable() -> bool
//! { false }` gets the right answer on Windows and the wrong one for a macOS
//! build running under Ghostty.
//!
//! **`TERM` is not the variable to gate on, and this box proves it.** Under Git
//! Bash inside Windows Terminal, `TERM` reads `xterm-256color` while
//! `TERM_PROGRAM` is unset and `WT_SESSION` is set — measured, 2026-09-06. A
//! gate that read `TERM` for the xterm family would have concluded that this
//! Windows Terminal honours xterm's `OSC 50` font sequence, which it does not.
//! `TERM` describes a *capability set a shell library should assume*; the
//! terminal's own identity is in `TERM_PROGRAM`, and Windows Terminal's is in
//! `WT_SESSION`. Those two are what [`detect`] reads.
//!
//! **In-band font control is not wired, deliberately.** xterm with Xft and urxvt
//! do accept a font in-band (`OSC 50` / `OSC 710`), and that would be the third
//! arm here. It is not implemented because nothing available to this project can
//! run it against a real xterm, and a sequence written blind is exactly the
//! class of confident-unverified claim this codebase has paid for before. It is
//! also the one arm with a second hazard: an escape sequence written to a
//! redirected stream is the regression `no_escape_bytes.rs` exists to catch, so
//! whoever adds it owes a terminal check on the stream *and* a test for the
//! pipe. Windows Terminal offers no such sequence at all, so nothing is lost on
//! this box by the omission.
//!
//! **The seam is the point.** [`command`] is pure and returns the argument
//! list, so a test names the script without running it; [`apply`] takes the
//! runner as an argument, so a test captures the call and no test in this
//! repository has ever spawned `osascript`.
//!
//! **What is claimed is "asked", never "set".** A font name Terminal does not
//! have is not an error there: AppleScript accepts the string and the window
//! silently keeps the font it had. Nothing available from this side can tell
//! that apart from success, so the receipt says what was asked and reports
//! whatever osascript said back. The one thing it never says is that the font
//! changed.
//!
//! **A terminal that offers nothing is told so, in a sentence.** The rows still
//! store the value and still show it, because the setting is Emma's and outlives
//! the terminal it was typed into — but where there is no control, the row prints
//! [`no_control_note`] instead of pretending a chevron does something. A control
//! that silently does nothing is worse than a sentence saying why there is none.

/// The `TERM_PROGRAM` value macOS Terminal.app exports for itself.
pub const APPLE_TERMINAL: &str = "Apple_Terminal";

/// The environment variable Windows Terminal sets, and the only reliable sign
/// of it: Windows Terminal does not set `TERM_PROGRAM`, and the `TERM` it
/// leaves behind is whatever the shell chose.
pub const WT_SESSION: &str = "WT_SESSION";

/// What Windows Terminal is called in a sentence a person reads.
pub const WINDOWS_TERMINAL: &str = "Windows Terminal";

/// How this build can reach the font of the terminal it is running in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Control {
    /// Terminal.app, driven by `osascript` from outside the process.
    AppleScript,
    /// The terminal exposes no font control Emma knows how to use. The value is
    /// still stored; nothing is asked of the terminal.
    None,
}

/// The terminal this process is running in, and what it will answer to.
///
/// Built by [`detect`] from the environment as data, so a test constructs one
/// directly and every caller downstream is platform-free.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Terminal {
    /// What to call it in a receipt. Empty when the environment says nothing,
    /// which [`Terminal::who`] renders as its own sentence rather than as a
    /// blank.
    pub name: String,
    /// What can be asked of it.
    pub control: Control,
}

impl Terminal {
    /// How a receipt names this terminal, including the case where it has no
    /// name to give.
    ///
    /// An unset `TERM_PROGRAM` is named as such rather than as "unknown",
    /// because it is the one case the reader can act on: a terminal that does
    /// not identify itself may still be scriptable, and knowing which variable
    /// was empty is the first step to finding out.
    pub fn who(&self) -> String {
        if self.name.is_empty() {
            "this terminal (TERM_PROGRAM is unset)".to_string()
        } else {
            format!("this terminal ({})", self.name)
        }
    }
}

/// Read the environment as data and say what terminal this is.
///
/// `TERM_PROGRAM` first, because a terminal that names itself is authoritative:
/// Terminal.app, iTerm, WezTerm, Ghostty and vscode all set it. `WT_SESSION`
/// second, because Windows Terminal sets no `TERM_PROGRAM` and would otherwise
/// be indistinguishable from a bare console. Neither is `TERM`; see the module
/// header for the measurement that rules `TERM` out.
pub fn detect(term_program: &str, wt_session: &str) -> Terminal {
    if term_program == APPLE_TERMINAL {
        return Terminal {
            name: "Terminal.app".to_string(),
            control: Control::AppleScript,
        };
    }
    if !term_program.is_empty() {
        return Terminal {
            name: term_program.to_string(),
            control: Control::None,
        };
    }
    if !wt_session.is_empty() {
        return Terminal {
            name: WINDOWS_TERMINAL.to_string(),
            control: Control::None,
        };
    }
    Terminal {
        name: String::new(),
        control: Control::None,
    }
}

/// [`detect`] against this process's own environment.
pub fn detect_here() -> Terminal {
    detect(&env_or_empty("TERM_PROGRAM"), &env_or_empty(WT_SESSION))
}

/// One environment variable, absent and empty treated alike — an exported but
/// empty `TERM_PROGRAM` says as little as an unset one.
fn env_or_empty(key: &str) -> String {
    std::env::var(key).unwrap_or_default()
}

/// Whether a row on the Settings screen has anything to drive.
///
/// The row is still drawn when this is false, and still stores its value; what
/// changes is that it prints [`no_control_note`] instead of offering a chevron
/// that would do nothing.
pub fn offers_control(terminal: &Terminal) -> bool {
    matches!(terminal.control, Control::AppleScript)
}

/// Font families the Font Family row is *seeded* with on a given platform.
///
/// **These are seeds, not an enumeration.** Nothing here has looked at what is
/// installed: reading the real font list costs a `system_profiler` run on macOS
/// measured in seconds, and a font-collection enumeration on Windows, which is
/// not a price a settings row may charge to draw itself. So the list is three
/// names that ship with the platform, the row's Add path writes any other name,
/// and a name the terminal does not have is reported by [`apply`] as *asked*
/// rather than as done. That failure reporting is what stands in for the
/// enumeration.
///
/// They are per-platform because the fork's three — Menlo, SF Mono, Monaco —
/// exist only on macOS, and offering them as the universal starting list would
/// seed every Windows and Linux row with three names guaranteed to be wrong.
pub fn seed_families(terminal: &Terminal, os: &str) -> &'static [&'static str] {
    /// macOS: Menlo has shipped since Snow Leopard and is Terminal.app's
    /// default, Monaco predates it and is still installed, SF Mono ships with
    /// the system and Xcode.
    const APPLE: [&str; 3] = ["Menlo", "SF Mono", "Monaco"];
    /// Windows: Cascadia Mono ships with Windows Terminal and is its default,
    /// Consolas has shipped since Vista, Lucida Console since Windows 2000.
    const WINDOWS: [&str; 3] = ["Cascadia Mono", "Consolas", "Lucida Console"];
    /// Elsewhere: the two families every mainstream distribution's core fonts
    /// package carries, plus the generic alias fontconfig always resolves.
    const OTHER: [&str; 3] = ["DejaVu Sans Mono", "Liberation Mono", "monospace"];

    // The terminal outranks the OS string, because it is the more specific
    // fact: Terminal.app only ever runs on macOS, and Windows Terminal only on
    // Windows, whatever a cross-compiled binary thinks it was built for.
    if terminal.control == Control::AppleScript {
        return &APPLE;
    }
    if terminal.name == WINDOWS_TERMINAL {
        return &WINDOWS;
    }
    match os {
        "macos" => &APPLE,
        "windows" => &WINDOWS,
        _ => &OTHER,
    }
}

/// [`seed_families`] for this process: this terminal, this build's target OS.
///
/// `std::env::consts::OS` rather than `#[cfg]` so the choice stays one branch
/// on a string that a test can drive both ways.
pub fn seed_families_here(terminal: &Terminal) -> &'static [&'static str] {
    seed_families(terminal, std::env::consts::OS)
}

/// What an absent `appearance.font_size` means: whatever the terminal already
/// had. Emma never asks for a size it was not told to ask for.
pub const DEFAULT_SIZE: u32 = 13;

/// The smallest size the row steps to, inclusive.
pub const MIN_SIZE: u32 = 8;
/// The largest size the row steps to, inclusive.
pub const MAX_SIZE: u32 = 32;

/// One thing to ask the terminal for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalFont {
    /// A font family, by the name the platform's font list spells it.
    Family(String),
    /// A size in points.
    Size(u32),
}

impl TerminalFont {
    /// What the receipt calls this, in the row's own words.
    pub fn what(&self) -> String {
        match self {
            Self::Family(name) => format!("font family {name}"),
            Self::Size(pt) => format!("font size {pt}"),
        }
    }
}

/// Step a size by one point, clamped rather than wrapped.
///
/// Clamped because a font size is a magnitude and not a ring: somebody holding
/// the right arrow at 32pt wants 32pt, not 8pt. Every other cycler on the
/// screen wraps, and this one saying so out loud is why the row's kind is its
/// own variant.
pub fn step_size(current: u32, dir: isize) -> u32 {
    current
        .saturating_add_signed(dir as i32)
        .clamp(MIN_SIZE, MAX_SIZE)
}

/// One ask as data: the command and its arguments, never a shell string.
///
/// `current settings of front window` is Terminal's own model: a window shows a
/// settings set (a profile), and the font lives on that set. Writing it changes
/// the front window and every other window sharing the profile, which is what a
/// person picking a font in the app itself would also get.
///
/// The font name is interpolated into the script rather than passed as an
/// argument because AppleScript has no argv for a `-e` snippet. Quotes and
/// backslashes are escaped for exactly that reason: a font name is a string
/// somebody typed, and an unescaped quote in it would end the literal and leave
/// the rest of the name as AppleScript source.
pub fn command(font: &TerminalFont) -> (&'static str, Vec<String>) {
    let body = match font {
        TerminalFont::Family(name) => format!(
            r#"tell application "Terminal" to set font name of current settings of front window to "{}""#,
            escape(name)
        ),
        TerminalFont::Size(pt) => format!(
            r#"tell application "Terminal" to set font size of current settings of front window to {pt}"#
        ),
    };
    ("osascript", vec!["-e".into(), body])
}

/// AppleScript string escaping: a backslash and a double quote, and nothing
/// else, because nothing else ends a literal.
fn escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// What a runner answers.
///
/// Deliberately **not** `std::process::Output`: constructing one in a test
/// needs an `ExitStatus`, and the only ways to build an `ExitStatus` from a
/// number are `std::os::unix::process::ExitStatusExt` and its Windows twin.
/// The fork used the unix one with no `cfg` and so did not compile here at all.
/// Two `cfg` arms would have compiled and would still have been the wrong
/// shape: what this module needs from a run is one bit and one string, and a
/// type that says so has no platform in it to get wrong.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ran {
    /// Whether the program reported success.
    pub ok: bool,
    /// What it wrote to stderr, trimmed. Empty is normal on success.
    pub said: String,
}

/// The Automation-permission fix, spelled the way the System Settings pane
/// spells it. macOS reports a denial as an osascript error rather than as a
/// prompt once the person has said no, which is why this has to be a sentence
/// somebody can act on rather than the raw code.
pub const AUTOMATION_FIX: &str = "macOS denied the Automation permission: System Settings > \
     Privacy & Security > Automation, then allow Terminal to control Terminal";

/// The sentence a Settings row prints where the terminal offers no font control.
///
/// It is a sentence and not a shrug for two reasons. It names the terminal,
/// because the person reading it is the one who knows whether that is worth
/// changing. And it says the value is kept, because the alternative reading —
/// that the row did not take the input — is the one that makes somebody type it
/// again.
pub fn no_control_note(terminal: &Terminal) -> String {
    format!(
        "{} offers no font control Emma can use, so nothing is asked of it. The value is \
         stored and applies wherever Emma next runs in a terminal that has one",
        terminal.who()
    )
}

/// What the receipt says when a row was pressed on a terminal offering nothing.
///
/// The same fact as [`no_control_note`], led by what was asked for, because a
/// receipt answers a press and the press was about one value.
pub fn not_controllable_note(font: &TerminalFont, terminal: &Terminal) -> String {
    format!("{} stored; {}", font.what(), no_control_note(terminal))
}

/// Ask the terminal for `font`, through `run`, and return the receipt.
///
/// `run` is the seam. Production passes [`spawn`]; a test passes a closure that
/// records the call, which is why nothing here has ever started a process under
/// `cargo test`.
pub fn apply(
    font: &TerminalFont,
    terminal: &Terminal,
    run: &mut dyn FnMut(&str, &[String]) -> std::io::Result<Ran>,
) -> String {
    match terminal.control {
        Control::None => not_controllable_note(font, terminal),
        Control::AppleScript => {
            let (cmd, args) = command(font);
            match run(cmd, &args) {
                Err(e) => format!(
                    "{} could not be asked for: {cmd} did not run ({e})",
                    font.what()
                ),
                Ok(ran) if ran.ok => format!(
                    "asked Terminal.app for {}. A name Terminal does not have is accepted and \
                     ignored there, so this says what was asked, not that the window changed",
                    font.what()
                ),
                Ok(ran) => {
                    let said = ran.said;
                    let fix =
                        if said.contains("-1743") || said.to_lowercase().contains("not allowed") {
                            format!(". {AUTOMATION_FIX}")
                        } else {
                            String::new()
                        };
                    if said.is_empty() {
                        format!(
                            "{} was refused by osascript, which said nothing{fix}",
                            font.what()
                        )
                    } else {
                        format!("{} was refused: osascript said {said}{fix}", font.what())
                    }
                }
            }
        }
    }
}

/// The one call the Settings glue makes: ask for `font` here, in this process,
/// and return the receipt.
///
/// **Under `cfg(test)` this never spawns anything.** Not "does not usually":
/// the test build has no path to [`spawn`] at all, and every call records into
/// [`captured`] instead. A seam a test has to remember to install is a seam some
/// test will forget, and the one thing this module must never do in CI is drive
/// somebody's terminal.
pub fn apply_here(font: &TerminalFont, terminal: &Terminal) -> String {
    #[cfg(test)]
    {
        let mut run = |cmd: &str, args: &[String]| {
            CAPTURE.with(|c| c.borrow_mut().push((cmd.to_string(), args.to_vec())));
            Ok(Ran {
                ok: true,
                said: String::new(),
            })
        };
        apply(font, terminal, &mut run)
    }
    #[cfg(not(test))]
    {
        let mut run = |cmd: &str, args: &[String]| spawn(cmd, args);
        apply(font, terminal, &mut run)
    }
}

#[cfg(test)]
thread_local! {
    /// Every ask [`apply_here`] made on this thread. Per-thread so tests that
    /// run in parallel cannot read each other's calls.
    static CAPTURE: std::cell::RefCell<Vec<(String, Vec<String>)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// What [`apply_here`] was asked for on this thread, newest last. Clears as it
/// reads, so a test asserts one press at a time.
#[cfg(test)]
pub fn captured() -> Vec<(String, Vec<String>)> {
    CAPTURE.with(|c| std::mem::take(&mut *c.borrow_mut()))
}

/// The production runner. Waited on rather than detached, because the whole
/// value of this call is what it says back.
pub fn spawn(cmd: &str, args: &[String]) -> std::io::Result<Ran> {
    let out = std::process::Command::new(cmd).args(args).output()?;
    Ok(Ran {
        ok: out.status.success(),
        said: String::from_utf8_lossy(&out.stderr).trim().to_string(),
    })
}

/// The startup re-apply: ask for whatever settings.json holds, and say nothing
/// whatever happens.
///
/// Best effort and silent by design. This runs before there is a screen to
/// print onto, and a terminal that refuses Automation would otherwise greet
/// every start with an error about a font. The Settings rows are where a
/// failure is worth reading, because that is where somebody just asked.
///
/// It goes through [`apply_here`] rather than building its own runner, and that
/// is not tidiness. The fork's version called [`spawn`] directly, which left its
/// own test unable to fail: on a machine with no `osascript` the spawn errors,
/// the error is discarded here by design, and the test that claimed to prove
/// "nothing is asked of a terminal it cannot drive" passed either way. Routed
/// through `apply_here`, the `cfg(test)` capture sees every ask this function
/// makes, and a mutation to [`apply`]'s refusal turns two tests red.
///
/// **There is deliberately no `offers_control` check here, and there was one
/// until a mutation ran.** It looked like defence in depth and was a second copy
/// of a decision `apply` already makes: deleting it left all eighteen tests
/// green, which is the definition of a line no test can see. One decision, one
/// place — the same argument this codebase makes about two readers with two
/// tolerances for one input shape.
pub fn restore(family: Option<&str>, size: Option<u32>, terminal: &Terminal) {
    if let Some(name) = family {
        let _ = apply_here(&TerminalFont::Family(name.to_string()), terminal);
    }
    if let Some(pt) = size {
        let _ = apply_here(&TerminalFont::Size(pt), terminal);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A runner that records instead of spawning. Every test in this module and
    /// in the settings glue uses one; none of them starts a process.
    fn recorder<'a>(
        calls: &'a mut Vec<(String, Vec<String>)>,
        ok: bool,
        said: &'static str,
    ) -> impl FnMut(&str, &[String]) -> std::io::Result<Ran> + 'a {
        move |cmd, args| {
            calls.push((cmd.to_string(), args.to_vec()));
            Ok(Ran {
                ok,
                said: said.to_string(),
            })
        }
    }

    fn apple() -> Terminal {
        detect(APPLE_TERMINAL, "")
    }

    #[test]
    fn terminal_app_is_recognised_and_is_the_only_thing_that_is() {
        assert_eq!(detect(APPLE_TERMINAL, "").control, Control::AppleScript);
        for other in [
            "iTerm.app",
            "WezTerm",
            "ghostty",
            "vscode",
            "Apple_Terminal_",
        ] {
            assert_eq!(detect(other, "").control, Control::None, "{other}");
        }
    }

    /// The variable Windows Terminal actually sets, and the name a person reads.
    #[test]
    fn windows_terminal_is_named_from_its_own_variable_and_offers_nothing() {
        let t = detect("", "e826f320-a761-4030-b1ae-1f185a8de887");
        assert_eq!(t.name, WINDOWS_TERMINAL);
        assert_eq!(t.control, Control::None);
        assert!(!offers_control(&t));
    }

    /// `TERM_PROGRAM` outranks `WT_SESSION`: Terminal.app inside a tmux inside
    /// nothing is still Terminal.app, and a terminal that names itself is the
    /// better evidence.
    #[test]
    fn a_terminal_that_names_itself_outranks_the_windows_terminal_variable() {
        assert_eq!(detect(APPLE_TERMINAL, "abc").control, Control::AppleScript);
        assert_eq!(detect("WezTerm", "abc").name, "WezTerm");
    }

    #[test]
    fn a_terminal_that_says_nothing_is_named_as_such_rather_than_left_blank() {
        let t = detect("", "");
        assert!(t.name.is_empty());
        assert!(t.who().contains("TERM_PROGRAM is unset"), "{}", t.who());
    }

    /// The seeds are seeds, and they are not the same three everywhere. The
    /// macOS three are what the fork shipped for every platform.
    #[test]
    fn the_seed_families_are_per_platform_and_never_the_apple_three_on_windows() {
        let apple_seeds = seed_families(&apple(), "macos");
        assert_eq!(apple_seeds, ["Menlo", "SF Mono", "Monaco"]);

        let wt = detect("", "abc");
        let win = seed_families(&wt, "windows");
        assert_eq!(win[0], "Cascadia Mono");
        for name in apple_seeds {
            assert!(
                !win.contains(name),
                "a macOS-only family seeded a Windows row: {name}"
            );
        }

        let unknown = detect("", "");
        assert_eq!(seed_families(&unknown, "linux")[0], "DejaVu Sans Mono");
        // The terminal is the more specific fact and outranks the OS string.
        assert_eq!(seed_families(&wt, "linux")[0], "Cascadia Mono");
        assert_eq!(seed_families(&apple(), "windows")[0], "Menlo");
    }

    #[test]
    fn the_family_script_targets_the_front_window_settings() {
        let (cmd, args) = command(&TerminalFont::Family("SF Mono".into()));
        assert_eq!(cmd, "osascript");
        assert_eq!(args[0], "-e");
        assert!(
            args[1].contains(r#"set font name of current settings of front window to "SF Mono""#)
        );
    }

    #[test]
    fn the_size_script_sends_a_number_and_not_a_string() {
        let (_, args) = command(&TerminalFont::Size(15));
        assert!(args[1].ends_with("to 15"), "{}", args[1]);
    }

    /// A font name is somebody's typing, and a quote in it must not become
    /// AppleScript source.
    #[test]
    fn a_quote_in_a_font_name_cannot_escape_the_literal() {
        let (_, args) = command(&TerminalFont::Family(r#"Ev"il"#.into()));
        assert!(args[1].contains(r#""Ev\"il""#), "{}", args[1]);
    }

    #[test]
    fn a_size_step_clamps_at_both_ends_rather_than_wrapping() {
        assert_eq!(step_size(13, 1), 14);
        assert_eq!(step_size(13, -1), 12);
        assert_eq!(step_size(MAX_SIZE, 1), MAX_SIZE);
        assert_eq!(step_size(MIN_SIZE, -1), MIN_SIZE);
    }

    #[test]
    fn a_size_step_applies_through_the_seam_and_captures_the_call() {
        let mut calls = Vec::new();
        let note = {
            let mut run = recorder(&mut calls, true, "");
            apply(&TerminalFont::Size(16), &apple(), &mut run)
        };
        assert_eq!(calls.len(), 1, "one ask per press");
        assert_eq!(calls[0].0, "osascript");
        assert!(calls[0].1[1].contains("font size"), "{}", calls[0].1[1]);
        assert!(calls[0].1[1].ends_with("to 16"));
        assert!(note.contains("asked Terminal.app"), "{note}");
        assert!(note.contains("not that the window changed"), "{note}");
    }

    #[test]
    fn a_terminal_this_build_cannot_drive_is_named_and_nothing_is_spawned() {
        let mut calls = Vec::new();
        let note = {
            let mut run = recorder(&mut calls, true, "");
            apply(
                &TerminalFont::Family("Menlo".into()),
                &detect("iTerm.app", ""),
                &mut run,
            )
        };
        assert!(
            calls.is_empty(),
            "nothing may be asked of a terminal that cannot answer"
        );
        assert!(
            note.contains("iTerm.app"),
            "the receipt names the terminal: {note}"
        );
        assert!(note.contains("offers no font control"), "{note}");
        assert!(note.contains("stored"), "the value is still kept: {note}");
    }

    /// The row on this box. It must be a sentence, it must name Windows
    /// Terminal, and it must not read as a failure to accept the value.
    #[test]
    fn the_windows_terminal_row_prints_a_sentence_and_not_a_dead_control() {
        let wt = detect("", "abc");
        let row = no_control_note(&wt);
        assert!(row.contains(WINDOWS_TERMINAL), "{row}");
        assert!(row.contains("offers no font control"), "{row}");
        // The whole promise, not just the word: a mutation that turned "The
        // value is stored" into "Nothing is stored" passed an assertion on
        // `contains("stored")` alone.
        assert!(row.contains("The value is stored"), "{row}");
        let receipt = not_controllable_note(&TerminalFont::Size(14), &wt);
        assert!(receipt.starts_with("font size 14 stored;"), "{receipt}");
    }

    #[test]
    fn an_automation_denial_carries_the_system_settings_fix() {
        let mut calls = Vec::new();
        let note = {
            let mut run = recorder(&mut calls, false, "execution error: Not allowed. (-1743)");
            apply(&TerminalFont::Size(20), &apple(), &mut run)
        };
        assert!(note.contains("Privacy & Security > Automation"), "{note}");
        assert!(
            note.contains("-1743"),
            "the receipt reports what osascript said: {note}"
        );
    }

    #[test]
    fn a_refusal_reports_what_osascript_said_and_never_claims_success() {
        let mut calls = Vec::new();
        let note = {
            let mut run = recorder(&mut calls, false, "syntax error: expected end of line");
            apply(&TerminalFont::Family("Nope".into()), &apple(), &mut run)
        };
        assert!(note.contains("refused"), "{note}");
        assert!(note.contains("syntax error"), "{note}");
        assert!(!note.contains("asked Terminal.app"), "{note}");
    }

    /// The startup path, proven by what it did rather than by returning.
    #[test]
    fn restore_asks_nothing_of_a_terminal_it_cannot_drive() {
        let _ = captured();
        restore(Some("Menlo"), Some(14), &detect("WezTerm", ""));
        restore(Some("Consolas"), Some(14), &detect("", "abc"));
        restore(Some("Menlo"), Some(14), &detect("", ""));
        assert!(
            captured().is_empty(),
            "startup asked something of a terminal that offers nothing"
        );
    }

    /// The other half of the same guarantee: on the one terminal that can be
    /// driven, startup asks for exactly what is stored, and only for that.
    #[test]
    fn restore_asks_for_each_stored_value_and_for_nothing_absent() {
        let _ = captured();
        restore(None, None, &apple());
        assert!(
            captured().is_empty(),
            "an absent setting is not a setting to re-apply"
        );

        restore(Some("Menlo"), None, &apple());
        let one = captured();
        assert_eq!(one.len(), 1);
        assert!(one[0].1[1].contains(r#"font name of current settings of front window to "Menlo""#));

        restore(Some("Menlo"), Some(14), &apple());
        let two = captured();
        assert_eq!(two.len(), 2, "family then size, one ask each");
        assert!(two[1].1[1].ends_with("to 14"), "{}", two[1].1[1]);
    }

    /// The one call the Settings glue makes, on the terminal this box actually
    /// has: it returns the sentence and records nothing, because there was
    /// nothing to record.
    #[test]
    fn apply_here_on_a_terminal_without_control_spawns_nothing_and_says_why() {
        let _ = captured();
        let note = apply_here(&TerminalFont::Family("Consolas".into()), &detect("", "abc"));
        assert!(captured().is_empty(), "no ask may be made: {note}");
        assert!(note.contains("offers no font control"), "{note}");
    }

    /// `detect_here` reads the real environment. What it finds depends on the
    /// box, so the assertion is the invariant that holds everywhere: whatever
    /// it says, the answer is self-consistent and printable.
    #[test]
    fn detect_here_agrees_with_the_environment_it_read() {
        let here = detect_here();
        assert_eq!(
            here,
            detect(&env_or_empty("TERM_PROGRAM"), &env_or_empty(WT_SESSION))
        );
        assert!(!here.who().is_empty());
        assert_eq!(offers_control(&here), here.control == Control::AppleScript);
        assert_eq!(seed_families_here(&here).len(), 3);
    }
}
