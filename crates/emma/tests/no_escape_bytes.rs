//! `INV-001`, asserted rather than believed.
//!
//! **`CLAUDE.md` said "Piped and `-p` output contains zero escape bytes. There
//! is a test. Keep it." An independent reviewer grepped for that test on
//! 2026-08-23 and there wasn't one.** What existed were component-level
//! assertions — `term/diff.rs`, `term/input.rs`, `term/statusline.rs` each check
//! their own renderer emits no escapes in plain mode — and a clipboard guard.
//! None of them runs the binary, and the invariant is about what comes out of
//! the process.
//!
//! So the guarantee rested on runtime receipts, which go stale, and on a
//! sentence in a house-rules file, which does not fail.
//!
//! **What this asserts is narrower than that sentence, deliberately.** The same
//! reviewer forced `Term::printing`'s `std::io::stderr().is_terminal()` to true
//! and got ten escape bytes on stderr from a `-p` run. That is not a defect: a
//! terminal attached to stderr is not somebody's pipeline, and colouring it is
//! the point of checking. The invariant that matters — and the one these
//! scenarios cover — is that **redirected** output carries no escape bytes,
//! because that is the stream that ends up in a file, a log, or the next
//! program.
//!
//! Every scenario here is keyless and makes no model call, so this costs
//! nothing and can run anywhere.
//!
//! **What it guards, measured rather than asserted.** Forcing
//! `Term::interactive`'s `std::io::stdout().is_terminal()` to `true` puts 44
//! escape bytes on a redirected stdout and turns this red, naming them:
//!
//! ```text
//! `emma ` put 44 escape byte(s) on a redirected stdout:
//! "\u{1b}[2;38;2;143;143;148m- \u{1b}[0m\u{1b}[2;38;2;143;143;148memma | claude-sonn…"
//! ```
//!
//! **What it does not guard, and cannot here.** The same mutation applied to
//! `Term::printing` — the `-p` constructor — leaves this green, because `-p`
//! requires a goal and therefore a model call, so no keyless scenario reaches
//! that writer at all. Six of the seven scenarios below go through a plain
//! `dyn Write` in `commands.rs` with no styling in it, which is why the seventh
//! exists; none of the seven can stand in for `-p`.
//!
//! The `-p` path's evidence is a runtime receipt instead
//! (`verification/receipts/def-057-runtime.json`: 71 seconds, 10 model calls,
//! four tool attempts, redirected to a file, zero escape bytes). That is a
//! measurement of one run rather than a guarantee, and closing the gap needs a
//! scripted provider the binary can be pointed at — which does not exist yet.

use std::io::Read;
use std::process::{Command, Stdio};

fn binary() -> std::path::PathBuf {
    // The test binary lives in target/<profile>/deps; the CLI is two up.
    let mut p = std::env::current_exe().expect("current_exe");
    p.pop();
    if p.ends_with("deps") {
        p.pop();
    }
    p.join(if cfg!(windows) { "emma.exe" } else { "emma" })
}

/// Run the CLI with both streams **redirected**, and hand back the raw bytes.
///
/// `Stdio::piped` is the whole point: it is what makes `is_terminal()` false on
/// both streams, which is the condition the invariant is about. Bytes rather
/// than a `String`, because an escape byte inside otherwise valid UTF-8 would
/// survive a lossy conversion but the count is what is being asserted and it
/// should not depend on a decoder.
fn piped(args: &[&str], env: &[(&str, &str)]) -> (Vec<u8>, Vec<u8>) {
    let exe = binary();
    if !exe.exists() {
        eprintln!("SKIPPED: {} is not built", exe.display());
        return (Vec::new(), Vec::new());
    }
    let mut cmd = Command::new(exe);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // A stale variable from the surrounding run must not decide the answer.
    cmd.env_remove("EMMA_NO_FRAME");
    cmd.env_remove("EMMA_UI");
    cmd.env_remove("EMMA_COLORS");
    for (k, v) in env {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().expect("spawn emma");
    let mut out = Vec::new();
    let mut err = Vec::new();
    child.stdout.take().unwrap().read_to_end(&mut out).unwrap();
    child.stderr.take().unwrap().read_to_end(&mut err).unwrap();
    let _ = child.wait();
    (out, err)
}

fn escapes(bytes: &[u8]) -> usize {
    bytes.iter().filter(|b| **b == 0x1b).count()
}

/// One invocation: the arguments, and the environment it needs.
type Scenario = (
    &'static [&'static str],
    &'static [(&'static str, &'static str)],
);

/// No redirected stream of a keyless run carries an escape byte.
///
/// The scenarios are chosen to reach different writers: `--help` is clap's own
/// output, `config check` is Emma's reporting path with notes and warnings in
/// it, `agents` walks the harness, and the two error cases go through the
/// usage-error path that exits non-zero. A single scenario would only prove
/// whichever writer it happened to touch.
#[test]
fn no_redirected_output_carries_an_escape_byte() {
    // A scratch session directory, so a test does not leave a transcript in the
    // user's real one every time it runs.
    let dir = std::env::temp_dir().join(format!("emma-inv001-{}", std::process::id()));
    let sessions = dir.to_string_lossy().into_owned();

    let scenarios: [Scenario; 6] = [
        (&["--help"], &[]),
        (&["--version"], &[]),
        (&["config", "check"], &[]),
        (&["agents"], &[]),
        // The usage-error path, which writes to stderr and exits 2.
        (&["config"], &[]),
        // A reporting path with something to complain about: a root that is not
        // a directory produces notes rather than a clean report.
        (
            &["config", "check"],
            &[("EMMA_ROOT", "definitely-not-a-directory")],
        ),
    ];

    let mut ran = 0;
    for (args, env) in scenarios {
        let (out, err) = piped(args, env);
        if out.is_empty() && err.is_empty() {
            continue;
        }
        ran += 1;
        assert_eq!(
            escapes(&out),
            0,
            "`emma {}` put {} escape byte(s) on a redirected stdout: {:?}",
            args.join(" "),
            escapes(&out),
            String::from_utf8_lossy(&out)
                .chars()
                .take(300)
                .collect::<String>()
        );
        assert_eq!(
            escapes(&err),
            0,
            "`emma {}` put {} escape byte(s) on a redirected stderr: {:?}",
            args.join(" "),
            escapes(&err),
            String::from_utf8_lossy(&err)
                .chars()
                .take(300)
                .collect::<String>()
        );
    }

    // **The one that reaches `Term`, and the reason the six above are not
    // enough.** Every one of them writes through a plain `dyn Write` in
    // `commands.rs`, which carries no styling at all — so forcing the colour
    // decision to `true` left all six green, and this file would have been a
    // receipt for nothing. A bare run prints the session header and the
    // interrupt hint through `Term`, reaches EOF on the closed stdin, and exits
    // without a model call: the only keyless path that goes through the writer
    // this invariant is about. Run here rather than in the array because its
    // environment is built at runtime.
    let (out, err) = piped(&[], &[("EMMA_SESSION_DIR", sessions.as_str())]);
    if !out.is_empty() || !err.is_empty() {
        ran += 1;
        assert_eq!(
            escapes(&out) + escapes(&err),
            0,
            "a bare interactive run put escape bytes on a redirected stream: {:?}",
            String::from_utf8_lossy(&out)
                .chars()
                .take(300)
                .collect::<String>()
        );
    }

    // **The anti-vacuity guard.** Every scenario producing nothing looks exactly
    // like every scenario passing, and this file exists because a test that
    // could not fail was taken for a guarantee.
    assert!(
        ran >= 5,
        "only {ran} of 7 scenarios produced any output; a run that writes nothing \
         cannot demonstrate that it writes no escape bytes"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// `EMMA_NO_FRAME` does not change the answer, and neither does a colour hint.
///
/// Separate from the sweep above because it is a different claim: those
/// scenarios show the plain path is clean, and this shows the plain path cannot
/// be talked out of being clean by the environment. `EMMA_COLORS=always` is the
/// interesting one — a user who asks for colour and then redirects has still
/// redirected.
#[test]
fn no_environment_setting_puts_escapes_on_a_redirected_stream() {
    for env in [
        vec![("EMMA_NO_FRAME", "1")],
        vec![("EMMA_COLORS", "always")],
        vec![("EMMA_NO_FRAME", "1"), ("EMMA_COLORS", "always")],
    ] {
        let (out, err) = piped(&["config", "check"], &env);
        if out.is_empty() && err.is_empty() {
            eprintln!("SKIPPED: no output for {env:?}");
            continue;
        }
        assert_eq!(
            escapes(&out) + escapes(&err),
            0,
            "{env:?} put escape bytes on a redirected stream: {:?}",
            String::from_utf8_lossy(&out)
                .chars()
                .take(300)
                .collect::<String>()
        );
    }
}
