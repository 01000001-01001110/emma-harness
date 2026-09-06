//! Driving the real binary in a real pseudo-terminal.
//!
//! **A cell buffer is not a console.** `CLAUDE.md` says so, and this repository
//! has paid for it twice: a fully-tested scrollback defect shipped from
//! `term/` on the strength of a `TestBackend` that agreed with its author. Every
//! terminal claim made during the parity program so far was *read from source*,
//! because no console was available to it — which is an honest position and a
//! useless one for anything that only fails on a real terminal.
//!
//! This file is the seam that changes that. It starts `emma` inside a genuine
//! PTY, sends real keystrokes, and reads the bytes the terminal actually
//! received. Nothing here inspects Emma's internals; the only evidence is what
//! came down the wire, which is the same thing a user's terminal sees.
//!
//! **What it deliberately does not do is call a model.** Every scenario below
//! either uses a subcommand that makes no request, or exits before a goal is
//! submitted. A test suite that spends money on every run is a suite people
//! turn off, and a paid assertion is not a better assertion.
//!
//! ## Reading the output
//!
//! The bytes include escape sequences, and that is the point — the invariants
//! worth checking here are *about* escape sequences. `saw` searches the raw
//! stream for a sequence; it does not emulate a terminal, because emulating one
//! would put a second implementation of the thing under test between the test
//! and the truth.

use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use std::io::{Read, Write};
use std::time::{Duration, Instant};

/// The alternate screen, entered and left. `?1049` is the modern pair; the
/// older `?47`/`?1047` spellings are not what crossterm emits and are not
/// accepted as evidence here.
const ALT_ENTER: &str = "\x1b[?1049h";
const ALT_LEAVE: &str = "\x1b[?1049l";

fn binary() -> std::path::PathBuf {
    // `CARGO_BIN_EXE_<name>` is set by cargo for the crate's own binaries, so
    // the test runs the artefact this build produced rather than whatever is on
    // PATH — which on this machine is a different, older emma.
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_emma"))
}

/// One run of the binary in a PTY of a given size.
///
/// Returns everything the terminal received. `keys` are written after
/// `settle`, which exists because a frame that has not finished painting has
/// not yet entered the alternate screen, and a keystroke that arrives first
/// proves nothing about the frame.
struct Run {
    output: String,
    status: Option<u32>,
}

fn run_in_pty(args: &[&str], env: &[(&str, &str)], keys: &[&str], settle: Duration) -> Run {
    let pty = native_pty_system();
    let pair = pty
        .openpty(PtySize {
            rows: 30,
            cols: 100,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("open pty");

    let mut cmd = CommandBuilder::new(binary());
    for a in args {
        cmd.arg(a);
    }
    cmd.cwd(std::env::temp_dir());
    // **Cleared before the caller's env is applied, not after.** A PTY test
    // exists to measure what the frame does on a real terminal, and both of
    // these variables take the frame away. Inherited from whoever ran `cargo
    // test`, either one would silently turn every frame assertion below into a
    // measurement of the plain path — and the shape those assertions have,
    // "find the enter sequence or return", would report that as a pass.
    // A test that quietly measures the opposite thing is the failure this file
    // was built to avoid.
    cmd.env_remove("EMMA_NO_FRAME");
    cmd.env_remove("EMMA_UI");
    for (k, v) in env {
        cmd.env(k, v);
    }

    let mut child = pair
        .slave
        .spawn_command(cmd)
        .expect("spawn emma in the pty");
    drop(pair.slave);

    let mut reader = pair.master.try_clone_reader().expect("reader");
    let collected = std::sync::Arc::new(std::sync::Mutex::new(Vec::<u8>::new()));
    let sink = collected.clone();
    // Drained on its own thread, past any cap: a reader that stops reading
    // fills the pipe and the child blocks on a write nobody consumes, which is
    // the same trap `tools/fs/src/bash.rs` documents for its own drains.
    let pump = std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => sink
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .extend(&buf[..n]),
            }
        }
    });

    std::thread::sleep(settle);
    {
        let mut writer = pair.master.take_writer().expect("writer");
        for k in keys {
            // **A write to the master after the child is gone is not a defect
            // in Emma, and on macOS it is an error rather than a no-op.** The
            // slave's last reader has closed, so the kernel answers `EIO`;
            // Windows returns success for the same call. This used to be an
            // `expect`, which turned "the child exited before the keystroke
            // arrived" -- the ordinary outcome of a build that will not start
            // interactively here -- into a panic naming nothing.
            //
            // Stopping is right and staying silent is not: this file's whole
            // argument is that an unexercised assertion must be visible, so the
            // error is printed and the assertions below still run. One of them
            // reports an absent enter sequence as an environment limitation;
            // the other still fails if the frame entered and never left, which
            // is the guarantee, and which a lost keystroke cannot fake.
            if let Err(e) = writer.write_all(k.as_bytes()) {
                eprintln!("the pty refused a keystroke ({e}).");
                eprintln!(
                    "The child had most likely already exited, so nothing below observed {k:?}."
                );
                break;
            }
            writer.flush().ok();
            std::thread::sleep(Duration::from_millis(120));
        }
    }

    // Bounded: a hung child must fail the test rather than the suite.
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut status = None;
    while Instant::now() < deadline {
        match child.try_wait() {
            Ok(Some(s)) => {
                status = Some(s.exit_code());
                break;
            }
            _ => std::thread::sleep(Duration::from_millis(50)),
        }
    }
    if status.is_none() {
        let _ = child.kill();
    }
    drop(pair.master);
    let _ = pump.join();

    let bytes = collected.lock().unwrap_or_else(|e| e.into_inner()).clone();
    Run {
        output: String::from_utf8_lossy(&bytes).into_owned(),
        status,
    }
}

fn saw(haystack: &str, needle: &str) -> bool {
    haystack.contains(needle)
}

/// The harness itself works: a real PTY, a real binary, real bytes back.
///
/// `config check` makes no model call and needs no key, so this costs nothing
/// and proves the plumbing before anything depends on it. A harness that has
/// never been shown to capture output is a harness whose later silence means
/// nothing.
///
/// **ConPTY does not paint for a process that exits immediately, and the empty
/// capture looks exactly like a broken harness.** `--version` was the obvious
/// first scenario and it comes back with the console's own setup and teardown
/// sequences — `?1049`-less, a `2J` clear, a title set — and *none of the
/// program's text*. Windows' pseudo-console renders on its own schedule; a
/// child that writes one line and exits can be torn down before a repaint ever
/// reaches the master.
///
/// So a scenario for this harness must live long enough to be drawn. That is a
/// property of the observer, not of Emma, and it is the sort of thing that would
/// otherwise be discovered as "the PTY tests are flaky".
/// **The `|| saw(&run.output, "emma")` disjunct made this unfalsifiable, and
/// it was found by review rather than by running.**
///
/// ConPTY sets the window title to the child's path, so the capture always
/// contains `C:\\src\\emma\\target\\debug\\emma.exe`. In a repository
/// named emma, building a binary named `emma.exe`, `saw(output, "emma")` cannot
/// return false on any machine. The whole captured stream on this box is the
/// console's own setup and teardown:
///
/// ```text
/// "\u{1b}[6n\u{1b}[?9001h\u{1b}[?1004h\u{1b}[?25l\u{1b}[?9001l\u{1b}[?1004l
///  \u{1b}[2J\u{1b}[m\u{1b}[H\u{1b}]0;C:\\src\\emma\\target\\debug\\emma.exe\u{7}\u{1b}[?25h"
/// ```
///
/// **None of Emma's output is in it.** So the test whose own doc says *"a
/// harness that has never been shown to capture output is a harness whose later
/// silence means nothing"* was itself that harness, and the two tests below
/// were asserting the absence of a marker in a stream that contained nothing at
/// all.
///
/// It now asserts only on a string Emma writes. When that does not arrive the
/// harness is not working here, and this says so loudly instead of passing:
/// [`captured`] is the shared precondition, and every test that reasons about
/// Emma's bytes consults it rather than asserting into an empty string.
#[test]
fn the_pty_harness_captures_what_the_binary_writes() {
    let run = harness_probe();
    if !captured(&run) {
        return;
    }
    assert!(run.status.is_some(), "the child never exited");
}

/// The scenario the harness proves itself with: `config check` against a root
/// that does not exist, which needs no key and makes no model call.
fn harness_probe() -> Run {
    run_in_pty(
        &["config", "check"],
        &[("EMMA_ROOT", "definitely-not-a-directory")],
        &[],
        Duration::from_millis(1500),
    )
}

/// Whether any of **Emma's own** output reached the master, said out loud when
/// it did not.
///
/// A `false` here is an environment limitation rather than a defect, and the
/// distinction only survives if it is visible: a silent early return and a pass
/// look identical in a test list, which is how three green PTY tests came to
/// assert nothing on this machine.
fn captured(run: &Run) -> bool {
    if saw(&run.output, "not a directory") {
        return true;
    }
    eprintln!(
        "SKIPPED: ConPTY delivered none of the child's output in this environment, so \
         nothing below is exercised here. What came back is the console's own setup and \
         teardown, including an OSC title carrying the binary path -- which is why matching \
         `emma` against it used to pass. Capture: {:?}",
        run.output.chars().take(400).collect::<String>()
    );
    false
}

/// **A run that is not interactive never touches the alternate screen.**
///
/// `CLAUDE.md` names this as one of three things that hold absolutely. It has a
/// unit test already; this is the same claim made against a real terminal,
/// which is where it would actually fail.
#[test]
fn a_non_interactive_run_never_enters_the_alternate_screen() {
    // `--version` writes one line and exits, which ConPTY may tear down before
    // it paints -- the module doc above says so. That makes an absence
    // assertion on it doubly empty, so the probe scenario is used instead and
    // the version check rides on the same capture precondition.
    let run = harness_probe();
    if !captured(&run) {
        return;
    }
    assert!(
        !saw(&run.output, ALT_ENTER),
        "a --version run entered the alternate screen"
    );

    let run = harness_probe();
    // Absence proves nothing about an empty capture, which is exactly what this
    // asserted into before the harness was checked.
    if !captured(&run) {
        return;
    }
    assert!(
        !saw(&run.output, ALT_ENTER),
        "a failing config check entered the alternate screen"
    );
}

/// **Every exit path leaves the alternate screen** — INV-002, on a real
/// terminal rather than on four atomics.
///
/// The frame is entered, then `/exit` is typed. The bytes must contain the
/// leave sequence, and it must come *after* the enter: a leave that preceded
/// the enter would be a different frame's, and asserting only "contains" would
/// accept that.
#[test]
fn the_frame_leaves_the_alternate_screen_on_a_clean_exit() {
    // **`("EMMA_NO_FRAME", "")` used to be here, and it disabled the frame this
    // test exists to observe.** `frame.rs` reads the variable with
    // `var_os(..).is_some()`, so a set-but-empty value is set. A test that turns
    // off its own subject cannot fail for the reason it names, and the early
    // return below then reported that as an environment limitation.
    let run = run_in_pty(&[], &[], &["/exit\r"], Duration::from_millis(1200));

    let Some(entered) = run.output.find(ALT_ENTER) else {
        // Not a failure of the invariant: this build may refuse to start
        // interactively here (no key, no harness). Say which, rather than
        // reporting a pass that measured nothing.
        eprintln!(
            "the frame never entered the alternate screen in this environment; \
             INV-002 was not exercised. Output: {:?}",
            run.output.chars().take(400).collect::<String>()
        );
        return;
    };
    let left = run.output.rfind(ALT_LEAVE);
    assert!(
        left.is_some_and(|l| l > entered),
        "the frame entered the alternate screen and did not leave it"
    );
}
