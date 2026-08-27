//! The guarantees of `frame.rs` and `app.rs`, asserted from outside them.
//!
//! **This module exists because those two files are being replaced.** The Mac
//! branch's versions are the base for the merged tree
//! (`notes/design/tui-fork-integration.md`), and
//! `notes/design/term-hardening-backport.md` counted what that costs: 65
//! hardening items, **57 of which had no test outside the file being
//! replaced**. A test living inside a file that is about to be overwritten is
//! deleted by the overwrite, and the guarantee it defended disappears with
//! nothing to announce it — the suite stays green because the assertion left
//! along with the code.
//!
//! The four that did survive were a thinner net than they looked. Two are in
//! `tests/pty.rs` and environment-gated: they print a skip and return, so an
//! absence of failure is not evidence. The two in `tests/no_escape_bytes.rs`
//! cannot see a tool's own bytes, because a keyless run never reaches a tool.
//!
//! So this file is the net. Everything here is written to go **red on
//! arrival** if the incoming version of `frame.rs` or `app.rs` lacks the fix,
//! rather than to pass quietly because the assertion was deleted too.
//!
//! # How these are written, and why not simply moved
//!
//! A test moved verbatim breaks on a rename and reads as a regression when it
//! is really a refactor. Two shapes are preferred here, in this order:
//!
//! 1. **Source-reading assertions**, where the guarantee is about *ordering* or
//!    *presence* — the panic hook installing before raw mode, a feature staying
//!    out of a manifest. These survive any rename, and they are the only way to
//!    assert an ordering that has no observable effect until the process dies.
//!    `frame.rs` already used this shape for the `scrolling-regions` check, and
//!    that test is one of `CLAUDE.md`'s named enforcers.
//! 2. **Behaviour through the narrowest surface that exists**, for everything
//!    else. Narrow because every item named here couples this file to the two
//!    being replaced, and a wide surface turns a legitimate refactor into a
//!    wall of red.
//!
//! **A compile error here is a success, not a mishap.** If the incoming
//! `frame.rs` has no `erase_frame`, this file stops compiling, and that is the
//! loudest possible signal that an item on the checklist needs a decision. Read
//! `notes/design/term-hardening-backport.md` and either re-apply the fix or
//! record why the item no longer applies.
//!
//! # What this file is not
//!
//! It is not a second copy of the `term/` test suite, and it must not grow into
//! one. Only guarantees that (a) somebody argued for in a commit or a lesson,
//! and (b) have no defender outside the replaced files, belong here. Everything
//! else stays where it is written.

// The items are added by the enforcer lift, one region per source file.

// region: frame.rs
// ---------------------------------------------------------------------------
// `frame.rs`'s share of the silent-loss list — §1a of
// `notes/design/term-hardening-backport.md`. Each test names its checklist id,
// the guarantee in behaviour terms, and the symptom it defends against.
//
// **Almost everything here reads source text, and that is a consequence of
// visibility rather than a preference.** `frame.rs` keeps the latches
// (`FRAME_ON`, `RAW_ON`, `ALT_ON`, `MOUSE_ON`, `CURSOR_ROW`), the mode
// constants, `erase_frame`, `synchronized`, `resize_target`, `anchor` and
// `MAX_INSERT_ROWS` private to itself. A sibling module cannot see one of
// them, so for those items the narrowest surface that exists is the file's
// own text. Where a public or `pub(crate)` surface does exist — `view_rows`,
// `Frame::memory_page_text`, `Frame::explorer_page_text` — the test goes
// through it and reads nothing.
//
// The source assertions are written to the shape the replaced file already
// used, with its two defences kept: every slice is bounded by markers that
// have to stay inside the function under test, and every `expect` says
// outright that a rename has made the assertion vacuous rather than letting
// it match nothing. Where a constant's *value* is what matters, the value is
// parsed out of the declaration and asserted — not grepped for as a spelling,
// which is the false receipt `frame.rs` already paid for once (§1a F12).
//
// One thing this file gets for free that `frame.rs` could not: it is not its
// own haystack. `frame.rs` had to assemble needles like `format!("?10{}h",
// "49")` because a literal in the test matched the test. Here the haystack is
// a different file, so the sequences are written out.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod frame_rs {
    use crate::term::frame::{view_rows, Frame};

    /// The file under guard. Read as text because nearly everything on §1a is
    /// private to it — see the region header.
    const SOURCE: &str = include_str!("frame.rs");

    /// emma's own manifest, for F13. Relative to this file, which sits beside
    /// `frame.rs`, so the path is the one `frame.rs` used.
    const MANIFEST: &str = include_str!("../../Cargo.toml");

    // -- reading `frame.rs` -------------------------------------------------

    /// The text between two markers, both of which must be inside the same
    /// function for the slice to mean anything.
    ///
    /// `what` is used in the panic messages so a rename reads as "this
    /// assertion is now vacuous" rather than as a mysterious index panic.
    fn body_between(open: &str, close: &str, what: &str) -> &'static str {
        let start = SOURCE
            .find(open)
            .unwrap_or_else(|| panic!("{what}: `{open}` is gone; this assertion is now vacuous"));
        let end = SOURCE[start..]
            .find(close)
            .unwrap_or_else(|| panic!("{what}: `{close}` is gone; this assertion is now vacuous"))
            + start;
        &SOURCE[start..end]
    }

    /// A function's body, from its signature to the first line that closes at
    /// column zero.
    fn fn_body(signature: &str) -> &'static str {
        body_between(signature, "\n}", signature)
    }

    /// Drop whole-line `//` comments.
    ///
    /// `frame.rs` argues about these sequences and this feature *by name*,
    /// repeatedly, to explain why they are what they are. Naming one is fine;
    /// writing one is the thing under test.
    fn code_only(text: &str) -> String {
        text.lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Turn a Rust string literal's source text into the bytes it stands for.
    ///
    /// Only the escapes `frame.rs` actually uses. An unknown escape is left
    /// alone rather than silently dropped, so a sequence this does not
    /// understand cannot quietly satisfy a `contains`.
    fn unescape(literal: &str) -> String {
        let mut out = String::new();
        let mut chars = literal.chars();
        while let Some(c) = chars.next() {
            if c != '\\' {
                out.push(c);
                continue;
            }
            match chars.next() {
                Some('x') => {
                    let hex: String = chars.by_ref().take(2).collect();
                    match u8::from_str_radix(&hex, 16) {
                        Ok(b) => out.push(b as char),
                        Err(_) => {
                            out.push_str("\\x");
                            out.push_str(&hex);
                        }
                    }
                }
                Some('r') => out.push('\r'),
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('0') => out.push('\0'),
                Some('\\') => out.push('\\'),
                Some('"') => out.push('"'),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        }
        out
    }

    /// Every `const NAME: &str = "…";` in `frame.rs`, by name, with the value
    /// it actually stands for.
    ///
    /// This is what lets the mode assertions below be about *values* rather
    /// than about how a line is written.
    fn str_consts() -> Vec<(String, String)> {
        let mut found = Vec::new();
        for line in SOURCE.lines() {
            let line = line.trim();
            let Some(rest) = line.strip_prefix("const ") else {
                continue;
            };
            let Some((name, tail)) = rest.split_once(": &str = ") else {
                continue;
            };
            let Some(lit) = tail.strip_suffix("\";").and_then(|t| t.strip_prefix('"')) else {
                continue;
            };
            found.push((name.to_string(), unescape(lit)));
        }
        assert!(
            found.len() >= 4,
            "no `const NAME: &str` declarations were found in frame.rs; every mode \
             assertion below is now vacuous"
        );
        found
    }

    /// The value of one named `&str` constant.
    fn str_const(name: &str) -> String {
        str_consts()
            .into_iter()
            .find(|(n, _)| n == name)
            .unwrap_or_else(|| panic!("`{name}` is gone from frame.rs; this assertion is vacuous"))
            .1
    }

    /// Every string literal in a slice of source, as the bytes it stands for.
    fn literals_in(text: &str) -> Vec<String> {
        let mut out = Vec::new();
        let bytes: Vec<char> = text.chars().collect();
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] != '"' {
                i += 1;
                continue;
            }
            let mut lit = String::new();
            i += 1;
            while i < bytes.len() && bytes[i] != '"' {
                if bytes[i] == '\\' && i + 1 < bytes.len() {
                    lit.push(bytes[i]);
                    lit.push(bytes[i + 1]);
                    i += 2;
                    continue;
                }
                lit.push(bytes[i]);
                i += 1;
            }
            i += 1;
            out.push(unescape(&lit));
        }
        out
    }

    /// What a mode function writes, resolved to bytes.
    ///
    /// Two shapes appear in `frame.rs`: a bare constant (`PASTE_ON.to_string()`)
    /// and a `format!` whose placeholders name constants
    /// (`format!("{PASTE_OFF}{SYNC_END}…")`). Both are resolved here, so the
    /// assertions read the sequences the function returns rather than the way
    /// somebody chose to spell it.
    fn sequences_written_by(signature: &str) -> String {
        let body = code_only(fn_body(signature));
        let consts = str_consts();
        let mut out = String::new();
        for literal in literals_in(&body) {
            let mut expanded = literal;
            for (name, value) in &consts {
                expanded = expanded.replace(&format!("{{{name}}}"), value);
            }
            out.push_str(&expanded);
        }
        for (name, value) in &consts {
            if body.contains(name.as_str()) {
                out.push_str(value);
            }
        }
        assert!(
            !out.is_empty(),
            "`{signature}` no longer writes any sequence this can resolve; the mode \
             pairing assertions are now vacuous"
        );
        out
    }

    /// The single string a mode function returns, with `{CONST}` placeholders
    /// resolved to their values.
    ///
    /// [`sequences_written_by`] is deliberately a bag of everything the body
    /// mentions, which is right for a pairing over mode numbers and wrong for
    /// anything about *order* — so the one assertion that is about order
    /// (`leave_modes` ends with the SGR reset) reads the literal itself.
    fn returned_string_of(signature: &str) -> String {
        let body = code_only(fn_body(signature));
        let literal = literals_in(&body)
            .into_iter()
            .next()
            .unwrap_or_else(|| panic!("`{signature}` returns no literal; assertion vacuous"));
        let mut expanded = literal;
        for (name, value) in str_consts() {
            expanded = expanded.replace(&format!("{{{name}}}"), &value);
        }
        expanded
    }

    /// Every private-mode number a string sets (`ESC [ ? N h`).
    fn modes_set_by(sequence: &str) -> Vec<String> {
        let mut out = Vec::new();
        for piece in sequence.split("\u{1b}[?").skip(1) {
            let digits: String = piece.chars().take_while(char::is_ascii_digit).collect();
            if !digits.is_empty() && piece[digits.len()..].starts_with('h') {
                out.push(digits);
            }
        }
        out
    }

    // -- F1, F21, F22: the order `install` does things in --------------------

    /// **The panic hook is installed before the first mode it has to undo, and
    /// the alternate screen's latch goes on before its bytes.** (F1, F21, F22)
    ///
    /// A panic in the window between entering a mode and installing the hook
    /// has *no hook to run*. No amount of forgiveness in the teardown helps,
    /// because nothing runs, and what the user is left holding is a shell that
    /// does not echo what they type, on a screen that is not theirs.
    ///
    /// **This is the item the incoming file already has wrong.** The audit
    /// (`notes/audits/2026-08-27-divergent-emma-fork.md` §2.6, quoted in §1a)
    /// puts `enable_raw_mode()` at its `:433`, `ALT_ON` at `:442` and the hook
    /// at `:478` — a forty-five-line unhooked window — and its own test
    /// *locates* the call without ever asserting its position, which is the
    /// false-receipt shape this file exists to replace. The record also has
    /// the live tree's version of the same regression: "a reviewer moved it
    /// back after `enable_raw_mode()` — the exact regression — and the whole
    /// workspace stayed green" (`fb11efe`).
    ///
    /// Two further orderings ride along, both of which §6b names as having no
    /// other mechanism available:
    ///
    /// - **F21** — `ALT_ON` is stored *before* the `ALT_ENTER` bytes go out.
    ///   The inverse looks harmless and is the stranding case: a panic between
    ///   the write and the latch leaves the alternate screen up with a restore
    ///   that refuses to run.
    /// - **F22** — `enter_modes()` is written *after* `FRAME_ON`, which is
    ///   asserted here as an absence: the slice stops at `FRAME_ON.store(true`,
    ///   so the call appearing inside it means it happens too early. Bracketed
    ///   paste has no latch of its own, so `FRAME_ON` is the only thing that
    ///   answers for it, and one enabled ahead of it is a shell that pastes
    ///   `ESC[200~` into itself forever.
    ///
    /// Source order, which this project treats as a last resort. What makes it
    /// defensible is that the property genuinely is an ordering of statements
    /// in one function with no runtime moment at which it can be observed:
    /// by the time a panic proves the hook was missing, the process is going
    /// down.
    #[test]
    fn the_panic_hook_goes_on_before_the_first_mode_it_undoes() {
        let body = body_between("pub fn install(", "FRAME_ON.store(true", "install");

        let hook = body
            .find("install_panic_hook();")
            .expect("install no longer installs the panic hook at all");
        let raw = body
            .find("enable_raw_mode()")
            .expect("install no longer enables raw mode; this assertion is now vacuous");
        let alt_latch = body
            .find("ALT_ON.store(true")
            .expect("install no longer latches the alternate screen; this assertion is vacuous");
        let alt_write = body
            .find("ALT_ENTER")
            .expect("install no longer writes the alternate-screen enter; assertion vacuous");

        assert!(
            hook < raw,
            "the panic hook is installed after raw mode is enabled. A panic in the window \
             between them has no hook to restore the terminal, and the user is left in a \
             shell that does not echo"
        );
        assert!(
            hook < alt_write,
            "the panic hook is installed after the alternate screen is entered — the \
             stranding CLAUDE.md names as the failure people uninstall over"
        );
        assert!(
            alt_latch < alt_write,
            "the alternate screen is entered before its latch is set, so a panic in \
             between strands it with a restore that refuses to run (F21)"
        );
        assert!(
            !body.contains("enter_modes("),
            "the terminal modes are written before FRAME_ON is set, so one enabled ahead \
             of it survives a panic in the next three lines (F22)"
        );
    }

    /// **`install` hands the terminal to ratatui without moving the cursor
    /// first.** (F16)
    ///
    /// The version this replaced walked the cursor down to `height -
    /// view_rows`, which put the box on the last row immediately and left
    /// every row it walked over blank — the owner sent a screenshot of two
    /// thirds of a window of nothing with a box under it.
    ///
    /// An absence, so there is nothing to observe at runtime and nothing for a
    /// cell buffer to witness: `install` needs a real terminal and refuses to
    /// run in a test binary. The end marker is `FRAME_ON.store(true` rather
    /// than `PANIC_HOOK.call_once`, which stopped working as one when the hook
    /// moved to the top of the function (`0b1cfd5`) — the slice then ran past
    /// the end of `install` and swept in unrelated code.
    #[test]
    fn install_hands_the_terminal_to_ratatui_without_moving_the_cursor_first() {
        let body = code_only(body_between(
            "pub fn install(",
            "FRAME_ON.store(true",
            "install",
        ));
        assert!(
            !body.contains("anchor("),
            "install anchors the frame to the bottom of the window again, which on a \
             fresh shell leaves every row it walked over blank"
        );
    }

    // -- F2, F3, F4: the teardown --------------------------------------------

    /// **Any one of the four latches is enough to make the teardown run.**
    /// (F2, F3)
    ///
    /// `FRAME_ON` is set *last*. The guard used to return early unless it was
    /// set, so a panic anywhere in install left raw mode, the alternate screen
    /// and mouse capture on with the only thing that turns them off refusing
    /// to run — "a shell with no echo, on a screen that is not the user's".
    /// The install-site comment claimed `ALT_ON` prevented it; it could not,
    /// because nothing ever reached the code that read it.
    ///
    /// `MOUSE_ON` is called out on its own because deleting just that conjunct
    /// left the whole lib suite green (`221a813`): the test that existed set
    /// three latches, and the guard needs only one to carry on. Capture left
    /// on means the inheriting shell has `?1000`/`?1006` enabled by a dead
    /// process — every click and every wheel notch types an escape sequence at
    /// the prompt, forever.
    ///
    /// The latches are private, so this reads the guard rather than driving
    /// it: everything before the early return has to mention all four.
    #[test]
    fn every_latch_alone_is_enough_to_make_the_teardown_run() {
        let body = code_only(fn_body("pub fn restore_terminal("));
        let guard_end = body
            .find("return;")
            .expect("restore_terminal has no early return; this assertion is now vacuous");
        let guard = &body[..guard_end];
        for latch in ["FRAME_ON", "RAW_ON", "ALT_ON", "MOUSE_ON"] {
            assert!(
                guard.contains(latch),
                "restore_terminal's early return does not consult {latch}, so a frame that \
                 got that far and no further is never torn down: {guard}"
            );
        }
    }

    /// **Mouse capture is disabled through crossterm's `DisableMouseCapture`,
    /// inside `restore_terminal`.** (F4)
    ///
    /// On Windows the enable was a console-mode change rather than bytes, so
    /// only the matching command undoes it — it cannot ride in `leave_modes`
    /// with the string modes.
    ///
    /// **The scoping is the fix, not the call.** The assertion this replaces
    /// searched all of `frame.rs`, which the `use` line at the top satisfies on
    /// its own, so deleting the disable from the restore path left it green
    /// (`461c43a`). Sliced to `restore_terminal`'s body, it cannot.
    #[test]
    fn mouse_capture_is_disabled_from_inside_the_restore_path() {
        let body = code_only(fn_body("pub fn restore_terminal("));
        assert!(
            body.contains("DisableMouseCapture"),
            "mouse capture is enabled and restore_terminal never disables it, so the \
             shell that inherits this terminal types escape sequences when it is clicked"
        );
    }

    /// **The inline erase climbs relatively and upwards only, and once.** (F6)
    ///
    /// Found by mutation: returning the empty string unconditionally left the
    /// whole lib suite green (`221a813`). On `EMMA_UI=inline` those bytes are
    /// the *entire* cleanup — there is no alternate screen to leave — so an
    /// empty string leaves the viewport painted on the shell the user gets
    /// back, with a prompt drawn over the top of it.
    ///
    /// Absolute addressing is refused: on a Windows console without VT the
    /// rows a program computes are window rows while `SetConsoleCursorPosition`
    /// reads screen-buffer rows, which is where a repaint aimed at "row 40" of
    /// a nine-thousand-row buffer goes to be invisible.
    ///
    /// The `CURSOR_ROW.swap` has to be *inside* this function rather than at
    /// its call site — that is what makes the second teardown (a `Drop` after
    /// the panic hook) write nothing instead of climbing three more rows into
    /// somebody's transcript. Asserted here as a scoping constraint, which is
    /// the kind a reindent destroys.
    #[test]
    fn the_inline_erase_climbs_upward_relatively_and_only_once() {
        let body = code_only(fn_body("fn erase_frame()"));
        let written: String = literals_in(&body).concat();
        assert!(
            written.contains('A'),
            "erase_frame no longer climbs to the frame's top row, so on the inline path \
             the viewport stays painted under the returning shell prompt: {written:?}"
        );
        assert!(
            written.contains("\r\u{1b}[J"),
            "erase_frame no longer wipes from the frame's top row to the bottom: \
             {written:?}"
        );
        assert!(
            !written.contains('H'),
            "erase_frame addresses a row absolutely; on a Windows console without VT \
             that is a screen-buffer row and the repaint is invisible: {written:?}"
        );
        assert!(
            !written.contains('B'),
            "erase_frame moves downward, into rows that are the terminal's: {written:?}"
        );
        assert!(
            body.contains("CURSOR_ROW.swap"),
            "erase_frame no longer consumes CURSOR_ROW, so a second teardown climbs \
             again — into the transcript above the frame: {body}"
        );
    }

    // -- F7, F8, F9, F10: the modes -----------------------------------------

    /// **Every `h` set on the way in has its `l` on the way out.** (F8)
    ///
    /// A private mode outlives the process. Bracketed paste left on means the
    /// inheriting shell receives `ESC[200~` around everything anybody pastes,
    /// forever, from a program that has exited.
    ///
    /// Asserted as a pairing over mode *numbers* rather than as literals, so
    /// that adding a mode to the way in without adding it to the way out fails
    /// here rather than on somebody's terminal. **This is why enter and leave
    /// are two functions rather than one with a `bool`**, and the shape is the
    /// item — a backport that keeps the strings and merges the functions
    /// leaves nothing for this to read.
    ///
    /// The two-in/two-out split is deliberate: `ALT_ENTER` is written before
    /// the `Terminal` is built and `ALT_LEAVE` only when `ALT_ENTER` was, so
    /// they cannot live in the mode functions.
    #[test]
    fn every_mode_set_on_the_way_in_is_unset_on_the_way_out() {
        let way_in = format!(
            "{}{}",
            sequences_written_by("fn enter_modes()"),
            str_const("ALT_ENTER")
        );
        let way_out = format!(
            "{}{}",
            sequences_written_by("fn leave_modes()"),
            str_const("ALT_LEAVE")
        );
        let modes = modes_set_by(&way_in);
        assert!(
            modes.len() >= 2,
            "the enter sequences set fewer modes than exist, so this pairing passes \
             vacuously: {way_in:?}"
        );
        for number in modes {
            assert!(
                way_out.contains(&format!("\u{1b}[?{number}l")),
                "mode {number} is set on the way in and never unset, so it outlives the \
                 process and belongs to whatever shell inherits the terminal: {way_out:?}"
            );
        }
    }

    /// **The way out shows the cursor, ends any synchronized update, and ends
    /// with `ESC[0m`.** (F7)
    ///
    /// A frame torn down mid-paint otherwise leaves a shell prompt bold,
    /// invisible, or — with the synchronized-update end unsent — not
    /// repainting until the terminal's own watchdog fires. The SGR reset is
    /// last, which is why this asserts `ends_with` rather than `contains`.
    #[test]
    fn the_way_out_restores_the_cursor_the_picture_and_the_attributes() {
        let leaving = returned_string_of("fn leave_modes()");
        assert!(
            leaving.contains("\u{1b}[?25h"),
            "the way out no longer shows the cursor: {leaving:?}"
        );
        assert!(
            leaving.contains(&str_const("SYNC_END")),
            "the way out no longer ends a synchronized update, so a frame torn down \
             mid-paint leaves the terminal holding its picture: {leaving:?}"
        );
        assert!(
            leaving.ends_with("\u{1b}[0m"),
            "the way out does not end by dropping every attribute, so the returning \
             shell prompt inherits whatever the last paint was wearing: {leaving:?}"
        );
    }

    /// **The alternate screen is entered with an explicit `2J` and `H`, and
    /// left with neither.** (F9, F10)
    ///
    /// Found by mutation: shortening `ALT_ENTER` to a bare `ESC[?1049h` left
    /// the whole lib suite green (`221a813`), because the pairing test above
    /// only ever reads mode *numbers* out of the constant and never what sits
    /// between them.
    ///
    /// xterm clears on entry, but that is a convention rather than DEC's
    /// promise — on a console that keeps the buffer, ratatui's first draw
    /// diffs against cells it believes are blank and old content shows through
    /// the first frame. `H` puts the cursor where ratatui's model assumes it
    /// is.
    ///
    /// The control matters as much as the assertion: an erase on the way *out*
    /// would wipe the shell content the mode's own restore exists to bring
    /// back.
    ///
    /// What this establishes is small and worth stating: the bytes Emma
    /// intends to send are the bytes in the constant. What any terminal does
    /// with them is a certification item, and the only way to run it is to
    /// open a console and look.
    #[test]
    fn the_alternate_screen_is_entered_with_an_explicit_clear() {
        let enter = str_const("ALT_ENTER");
        assert!(
            enter.starts_with("\u{1b}[?1049h"),
            "the switch to the alternate screen is no longer the first thing sent: \
             {enter:?}"
        );
        assert!(
            enter.contains("\u{1b}[2J"),
            "the alternate screen is entered without erasing it, so a console that does \
             not clear on 1049h shows its old contents through the first frame: {enter:?}"
        );
        assert!(
            enter.ends_with("\u{1b}[H"),
            "the cursor is left where the switch put it rather than at the origin \
             ratatui's first diff assumes: {enter:?}"
        );
        assert_eq!(
            str_const("ALT_LEAVE"),
            "\u{1b}[?1049l",
            "the alternate-screen leave carries more than the mode reset; an erase here \
             wipes the shell content the restore exists to bring back"
        );
    }

    // -- F11, F12: synchronized output ---------------------------------------

    /// **A synchronized update is ended on every draw, not only on the way
    /// out.** (F11, F12)
    ///
    /// Found by mutation: deleting `out.write_all(SYNC_END.as_bytes())` left
    /// the whole lib suite green (`221a813`). The exit-path pairing above
    /// fires once; this fires on every single draw. A terminal told to hold
    /// its picture and never released holds it until its watchdog fires —
    /// Emma redrawing at whatever rate the terminal allows.
    ///
    /// **`synchronized` has no seam.** It writes to the process's real stdout
    /// and returns only its closure's value, so a behavioural test needs a
    /// `Write` parameter on production code. The source slice is the stated
    /// last resort; the counting form is used rather than a spelling grep
    /// because the version before it asserted
    /// `contains("out.push_str(SYNC_END)")` — a literal that occurred exactly
    /// once, *inside the assertion itself* — so it matched its own source and
    /// could not fail (`67aa70e`).
    ///
    /// F12 rides along at the value level: begin and end must be an `h`/`l`
    /// pair on one mode number, because a typo in either leaves a terminal
    /// holding its picture forever.
    #[test]
    fn a_synchronized_update_is_always_ended() {
        let begin = str_const("SYNC_BEGIN");
        let end = str_const("SYNC_END");
        assert_eq!(
            begin, "\u{1b}[?2026h",
            "the synchronized-update mode changed"
        );
        assert_eq!(
            begin.replace('h', "l"),
            end,
            "the synchronized update begins on one mode number and ends on another, so \
             the terminal holds its picture until its own timeout fires: {begin:?} {end:?}"
        );

        let body = code_only(fn_body("fn synchronized<T>("));
        let begins = body.matches("SYNC_BEGIN").count();
        let ends = body.matches("SYNC_END").count();
        assert_eq!(
            begins, 1,
            "synchronized no longer begins exactly one update: {body}"
        );
        assert_eq!(
            ends, begins,
            "synchronized begins an update it does not end, so every draw leaves the \
             terminal holding its picture until its own timeout fires: {body}"
        );
    }

    // -- F13: the named enforcer ---------------------------------------------

    /// **The `scrolling-regions` ratatui feature is never declared; exactly two
    /// viewports are constructed, both in `frame.rs`; the alternate screen is
    /// entered and left in one place each.** (F13)
    ///
    /// One of `CLAUDE.md`'s named enforcing tests. With the feature on, the
    /// inline path's `insert_before` becomes a DEC scroll region whose top
    /// margin below row 1 *discards* the lines that leave it. Not a theory: it
    /// shipped here once and the owner's first complaint was that he could not
    /// scroll. A third `Viewport::` construction is a path around the install
    /// gate; a second `?1049h` is an entry the restore latch does not know
    /// about.
    ///
    /// Comments are stripped from both haystacks because the manifest and
    /// `frame.rs` both *name* the feature and the sequence to explain why they
    /// are what they are. Naming is fine; declaring is not.
    ///
    /// **Known gap, carried over from `461c43a`:** this scans only emma's own
    /// manifest, so Cargo feature unification from another ratatui dependent
    /// would bypass it. Latent today — no other workspace crate depends on
    /// ratatui.
    #[test]
    fn the_scrolling_regions_feature_is_never_enabled() {
        let manifest = code_only(
            &MANIFEST
                .lines()
                .filter(|l| !l.trim_start().starts_with('#'))
                .collect::<Vec<_>>()
                .join("\n"),
        );
        assert!(
            !manifest.contains("scrolling-regions"),
            "ratatui's scrolling-regions feature would implement the inline path's \
             insert_before with a DEC scroll region, which discards scrollback. The \
             inline escape hatch still ships; the feature stays off."
        );

        assert!(
            SOURCE.contains("Viewport::"),
            "frame.rs constructs no viewport at all; this assertion is now vacuous"
        );
        assert!(
            SOURCE
                .split("Viewport::")
                .skip(1)
                .all(|rest| rest.starts_with("Inline") || rest.starts_with("Fullscreen")),
            "a viewport beyond the sanctioned two is constructed in frame.rs"
        );

        let code = code_only(SOURCE);
        assert_eq!(
            code.matches("?1049h").count(),
            1,
            "the alternate screen is entered somewhere other than ALT_ENTER, which is an \
             entry the restore latch does not know about"
        );
        assert_eq!(
            code.matches("?1049l").count(),
            1,
            "the alternate-screen leave exists somewhere other than ALT_LEAVE"
        );
    }

    // -- F14: the insert cap -------------------------------------------------

    /// **One transcript insert is capped at 500 rows.** (F14)
    ///
    /// Found by mutation: raising it to `u16::MAX` left the whole lib suite
    /// green (`221a813`). A tool returning fifty thousand lines would
    /// otherwise ask the terminal for fifty thousand rows of room in one call.
    ///
    /// **The bound is the literal, and that is the point.** The assertion that
    /// existed before this one was written as `scrolled <= MAX_INSERT_ROWS`,
    /// which passes for every value the constant can hold, including
    /// `u16::MAX` — measured. `MAX_INSERT_ROWS` is private, so this reads the
    /// declaration and asserts the number it is declared at; a value that no
    /// longer parses as a number (`u16::MAX`) fails on the `expect`.
    #[test]
    fn one_insert_is_capped_at_a_number_that_is_still_a_cap() {
        let line = SOURCE
            .lines()
            .map(str::trim)
            .find(|l| l.starts_with("const MAX_INSERT_ROWS"))
            .expect("the insert cap is gone from frame.rs; this assertion is now vacuous");
        let value = line
            .split_once('=')
            .map(|(_, v)| v.trim().trim_end_matches(';').trim())
            .expect("the insert cap has no value; this assertion is now vacuous");
        let rows: u16 = value.parse().unwrap_or_else(|_| {
            panic!(
                "the insert cap is {value}, which is not a plain row count. A tool \
                 returning fifty thousand lines now asks the terminal for fifty \
                 thousand rows of room in one call"
            )
        });
        assert!(
            rows > 0,
            "the insert cap is zero, which is a refusal rather than a cap: dropping the \
             block loses the line that says what went wrong"
        );
        assert!(
            rows <= 500,
            "the insert cap is {rows} rows; the documented cap is 500, and the value is \
             the whole of the guarantee"
        );
    }

    // -- F17, F18: the resize path -------------------------------------------

    /// **A resize puts a pinned frame back on the bottom and leaves an
    /// unpinned one under its own output.** (F17)
    ///
    /// `None` is not "do nothing safe": it is the answer for a frame still
    /// sitting under the two lines of output it belongs to. An unconditional
    /// re-anchor on a window dragged taller drops the box thirty rows below
    /// them — the startup void arriving through the resize path. A latch
    /// rather than a comparison, because a taller window makes the old bottom
    /// row an ordinary middle row and there is nothing left on screen to work
    /// it out from.
    ///
    /// **`resize_target` exists as a separate function because `reanchor`
    /// writes to the process's real stdout and cannot be called from a test
    /// binary.** If the backport inlines it, the guarantee becomes untestable
    /// — which is why the existence of the function is asserted here as well
    /// as what it does with `pinned`.
    ///
    /// The mutation this must catch is
    /// `fn resize_target(_, rows, h) -> Option<u16> { Some(anchor_row(rows, h)) }`,
    /// which leaves `pinned` unconsulted.
    #[test]
    fn only_a_pinned_frame_is_moved_by_a_resize() {
        let body = code_only(fn_body("fn resize_target("));
        assert!(
            body.contains("-> Option<u16>"),
            "resize_target no longer answers `None` for a frame that has not reached the \
             bottom, so a taller window drags it there and reopens the startup void: \
             {body}"
        );
        let after_signature = body
            .split_once(')')
            .map(|(_, rest)| rest)
            .expect("resize_target has no parameter list; this assertion is now vacuous");
        assert!(
            after_signature.contains("pinned"),
            "resize_target's body does not consult `pinned`, so every resize re-anchors \
             and a frame sitting under two lines of output is dragged to the bottom: \
             {body}"
        );
    }

    /// **`anchor` walks down with `append_lines`, up with absolute addressing,
    /// and refuses to anchor at all when the backend cannot report its
    /// cursor.** (F18)
    ///
    /// Walking down is newlines over rows that already exist, which scrolls
    /// nothing; walking up is absolute, and is safe only over rows
    /// `erase_frame` has just cleared. A guessed cursor position writes into
    /// the transcript, which is the terminal's and not Emma's.
    #[test]
    fn anchoring_walks_down_by_newline_and_refuses_to_guess() {
        let body = code_only(fn_body("fn anchor<B: Backend>("));
        assert!(
            body.contains("append_lines"),
            "anchor no longer walks down with newlines; anything else scrolls rows that \
             already exist: {body}"
        );
        assert!(
            body.contains("get_cursor_position"),
            "anchor no longer asks where the cursor is, so it is guessing: {body}"
        );
        assert!(
            body.contains("return"),
            "anchor no longer bails out when the backend cannot report its cursor, so a \
             guessed position writes into the transcript above the frame: {body}"
        );
    }

    // -- F19: the viewport height (a real behavioural test) -------------------

    /// **Viewport height is a third of the window, clamped to 5..=10.** (F19)
    ///
    /// Ten rows holds a status line, a hint, an input box and five rows of
    /// diff; five is the least that can hold a question and the keys to answer
    /// it. Below eight rows `fallback_reason` refuses outright, and that floor
    /// is tested in `term.rs`, which survives the replacement.
    ///
    /// The one item on §1a with a public surface, so this one is behaviour and
    /// reads no source at all.
    #[test]
    fn the_viewport_is_a_third_of_the_window_within_reason() {
        assert_eq!(view_rows(40), 10);
        assert_eq!(view_rows(24), 8);
        assert_eq!(
            view_rows(9),
            5,
            "the floor is the least that holds a question and its keys"
        );
        assert_eq!(
            view_rows(120),
            10,
            "the ceiling stops the frame eating a tall window"
        );
    }

    // -- F32: the page failure arms ------------------------------------------

    /// **A page that cannot read its store says so, and says which store.**
    /// (F32)
    ///
    /// Both arms lived inside `launch_tool`, which needs a real terminal, so
    /// `"nothing could be read"` existed in exactly one place in the
    /// repository: the line that produces it. "A page rendering an error and a
    /// page rendering nothing look similar and mean opposite things" — *there
    /// is nothing here* and *something went wrong* are different instructions
    /// to whoever is reading. And the two pages read different stores, so a
    /// shared sentence would leave a reader unable to tell which failed.
    ///
    /// **`None`, not a missing directory, and that difference is the test.**
    /// The first version passed `Some("definitely-not-a-directory")` and both
    /// mutants survived it: `commands::capture_memory` treats an absent
    /// directory as an *empty store* and returns `Ok`, so the error arm was
    /// never reached (`bd280d5`).
    ///
    /// **The extraction is the item.** These are `pub(crate) fn` on `Frame`
    /// precisely so a test can call them without a terminal; a backport that
    /// folds them back into `launch_tool` is a regression even if the strings
    /// survive — and it takes this file's compilation with it, which is the
    /// loudest signal available.
    #[test]
    fn a_page_that_cannot_read_its_store_says_which_store() {
        let memory = Frame::memory_page_text(None, std::path::Path::new("."));
        let explorer = Frame::explorer_page_text(None);

        assert!(
            !memory.trim().is_empty(),
            "the memory page came back blank when its store could not be read, which \
             reads as an empty store — the opposite claim"
        );
        assert!(
            !explorer.trim().is_empty(),
            "the explorer page came back blank when its store could not be read"
        );
        assert_ne!(
            memory, explorer,
            "both pages say the same thing, so a reader cannot tell which store could \
             not be read"
        );
    }

    /// The positive half of the pair above: the Memory page's failure sentence
    /// names a failure rather than a count. (F32)
    ///
    /// Without it, two unhelpful-but-different strings satisfy the assertion
    /// that they differ.
    #[test]
    fn the_memory_pages_failure_says_something_went_wrong() {
        let text = Frame::memory_page_text(None, std::path::Path::new("."));
        assert!(
            text.contains("nothing could be read"),
            "the memory page did not report the failure it hit; a blank or bland page \
             reads as an empty store, which is the opposite claim: {text:?}"
        );
    }
}

// endregion: frame.rs

// region: app.rs
// ---------------------------------------------------------------------------
// `app.rs` — the 25 items of `notes/design/term-hardening-backport.md` §2a.
//
// **Every one of the 25 had its only test inside `app.rs`.** Not one of them
// had a defender anywhere else in the workspace, which makes this file the
// whole net for the full-screen layout: what it draws, what it refuses to
// draw, and which of the keys it advertises are keys that do something.
//
// These are written to the *guarantee*, not copied. `app.rs`'s own tests
// assert exact sentences the current pages happen to emit; the incoming
// version is different code meaning to do the same job, and an exact-string
// assertion on it fails for the wrong reason. So where a property was
// available it is asserted instead: that a page is a function of the run
// rather than of a constant, that a wrap width is exactly the pane's message
// column, that every chord the sidebar renders is one the decoder returns.
//
// Where a test still had to be tied to current wording, its doc says so, in
// the words "expect to adjust on arrival". That is not a regression when it
// fires; a silent pass would be.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod app_rs {
    use ratatui::buffer::Buffer;
    use ratatui::layout::{Position, Rect};
    use ratatui::text::Line;

    use super::super::app::{
        dock_height, hidden, regions, stem, tool_rows, App, Latch, Page, Pane, Regions,
    };
    use super::super::palette::{Level, Palette};
    use super::super::render::{Skin, UNICODE};
    use super::super::view::{Mode, Prompt, View};
    use super::super::{chat, sidebar, statusbar};

    fn skin() -> Skin {
        Skin::new(Palette::new(Level::Truecolor), UNICODE)
    }

    /// A run with every status field set to something this file can recognise
    /// again on screen.
    fn view() -> View {
        let mut v = View::new(skin());
        v.status.model = "claude-opus-4".into();
        v.status.cwd = "C:\\src\\emma".into();
        v.status.session = "C:/Users/you/.emma/sessions/2026-08-23T11-02-55.jsonl".into();
        v.status.context = Some((0, 120_000));
        v.status.spend = Some((0, 500_000));
        v
    }

    fn bar() -> statusbar::Bar {
        statusbar::Bar {
            mode: "IDLE".into(),
            model: "claude-opus-4".into(),
            env: "emma".into(),
            ctx_used: 0,
            ctx_max: 120_000,
            total_used: 0,
            total_max: 500_000,
            up: None,
            down: None,
            elapsed: None,
        }
    }

    /// One TOOLS row, because `App::new` seeds that section with the slash
    /// commands as a placeholder and the shell replaces it before the first
    /// paint. Drawing the placeholder tests a state nobody sees, and it is
    /// length-sensitive: adding two slash commands once made the untouched
    /// sidebar tall enough to push QUICK HELP off a 40-row window.
    fn one_tool() -> Vec<sidebar::Row> {
        vec![sidebar::Row {
            name: "Shell".into(),
            trailing: "Alt+s".into(),
            selected: false,
        }]
    }

    /// The window as cells, one string per row, **untrimmed** — column
    /// positions have to survive, because several of these assertions are
    /// about which *region* a string landed in rather than whether it is on
    /// screen at all.
    fn cells(app: &mut App, v: &View, w: u16, h: u16) -> (Vec<String>, Option<Position>) {
        let area = Rect::new(0, 0, w, h);
        let mut buf = Buffer::empty(area);
        let cursor = app.render(area, &mut buf, v, &bar());
        let rows = (0..h)
            .map(|y| (0..w).map(|x| buf[(x, y)].symbol().to_string()).collect())
            .collect();
        (rows, cursor)
    }

    /// The carve this file believes was used for a paint of `w`×`h`, for tests
    /// that need to know which cells were which region's.
    fn layout_for(v: &View, w: u16, h: u16) -> Regions {
        let latch = Latch::default();
        regions(
            Rect::new(0, 0, w, h),
            sidebar::width(w, hidden(w, latch)),
            dock_height(v, h),
        )
    }

    /// Just the main pane's transcript/page rectangle, as lines.
    ///
    /// Every "is it on the page" assertion below reads this rather than the
    /// whole screen. A `contains` over the whole window is satisfied by the
    /// sidebar's QUICK HELP or by the status bar three rows down, and that has
    /// already made one assertion here vacuous.
    fn pane_rows(rows: &[String], r: &Regions) -> Vec<String> {
        (r.chat.y..r.chat.bottom())
            .map(|y| {
                rows[usize::from(y)]
                    .chars()
                    .skip(usize::from(r.chat.x))
                    .take(usize::from(r.chat.width))
                    .collect::<String>()
            })
            .collect()
    }

    fn pane_text(rows: &[String], r: &Regions) -> String {
        pane_rows(rows, r).join("\n")
    }

    /// Render one pane at `w`×`h` and return the whole screen and the carve.
    fn page_screen(v: &View, pane: Pane, w: u16, h: u16) -> (Vec<String>, Regions) {
        let mut a = App::new((w, h));
        a.set_tools(one_tool());
        match pane {
            // Five lines, not one: the Data Explorer's page is its snapshot
            // plus a footer, so a one-line snapshot fits in any window and the
            // out-of-room case (A1) becomes unreachable through this helper.
            Pane::Page(Page::DataExplorer) => a.show_explorer(
                "source         C:/store\nsessions       3 file(s)\n\nkinds\n  goal   3\n",
            ),
            Pane::Page(Page::Memory) => a.show_memory("project  C:/src/emma"),
            other => a.show(other),
        }
        let (rows, _) = cells(&mut a, v, w, h);
        (rows, layout_for(v, w, h))
    }

    // -----------------------------------------------------------------------
    // A17 / A16 — the chords the sidebar advertises, and the decoder
    // -----------------------------------------------------------------------

    /// **A17.** Every chord the TOOLS panel renders is a chord the key decoder
    /// returns, and the key column never carries a bare letter.
    ///
    /// A rendered key that does nothing is the defect the design's §6 forbids,
    /// and this project has paid for it twice. Once in the code: the sidebar
    /// showed `Alt+M` while `launch_tool` handed the key to `launch`, which
    /// refused it with a warning about a key the frame had never claimed. Once
    /// in a bug report: the owner reported the `Alt` chords not opening the
    /// tool pages, twice, using the word *"still"*, and the whole investigation
    /// went into a defect that did not exist. A test that a drawn affordance
    /// has a live handler is what answers that in one run.
    ///
    /// DEF-040 is the direction nobody checks: `catalogue_on` still routed
    /// Settings and the Data Explorer through `plan()` after they became pages,
    /// so **on a machine with no editor and no file manager the sidebar would
    /// have shown `n/a` beside two chords that work.** On the machine that built
    /// it both programs were installed and nothing looked wrong.
    ///
    /// Written as a property over the **real catalogue** rather than the three
    /// pages, because the same shape is `notes/design/tui-fork-inventory.md`
    /// §11's finding about the incoming tree's QUICK HELP panel — four of six
    /// advertised keys resolve to no handler at all. The implication asserted
    /// here (advertised ⇒ decodable) holds however many tools the catalogue
    /// grows, and does not care which of them the machine can launch. The three
    /// in-app pages are then pinned by name, because `Tool::routed()` never
    /// probes the box for those, so their availability is deterministic on any
    /// developer's machine.
    ///
    /// It stops at the decoder. **Nothing here proves a terminal delivers
    /// `Alt+,`** — that is a standing certification item, and `KeyModifiers::ALT`
    /// is indistinguishable from `ESC` then `s` in a buffer.
    #[test]
    fn every_chord_the_sidebar_advertises_is_one_the_decoder_returns() {
        use super::super::input::tool_key;
        use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let entries = crate::usertools::catalogue(std::path::Path::new("."));
        let rows = tool_rows(&entries);
        assert_eq!(
            rows.len(),
            entries.len(),
            "the sidebar's TOOLS section is not the catalogue: a dropped entry is a \
             tool with no way in, an invented one is a key that does nothing"
        );

        let mut chords = 0usize;
        for (entry, row) in entries.iter().zip(&rows) {
            let key_cell = row.trailing.as_str();
            if let Some(rest) = key_cell.strip_prefix("Alt+") {
                let mut chars = rest.chars();
                let key = chars.next().unwrap_or_else(|| {
                    panic!("{} advertises a bare `Alt+` with no key", entry.label)
                });
                assert!(
                    chars.next().is_none(),
                    "{} advertises {key_cell:?}, which is not one chord",
                    entry.label
                );
                assert_eq!(
                    tool_key(KeyEvent::new(KeyCode::Char(key), KeyModifiers::ALT), false),
                    Some(key),
                    "the sidebar advertises {key_cell} for {} and the decoder drops it: \
                     a rendered key that does nothing",
                    entry.label
                );
                chords += 1;
            } else {
                assert!(
                    key_cell == "n/a" || key_cell == "/",
                    "{} shows {key_cell:?} in the key column. That is neither a chord, \
                     nor the `/` the character already opens, nor the `n/a` an \
                     unavailable tool gets — and a bare letter is not a binding here, \
                     because the first character of every goal lands on an empty editor",
                    entry.label
                );
            }
        }
        assert!(
            chords >= 3,
            "no chord was checked, so the loop above proves nothing: the catalogue \
             advertised {chords} `Alt+` chords"
        );

        // The three in-app pages, by name. These are the deterministic half —
        // `Tool::routed()` decides their availability and never probes the box.
        for (label, key) in [("Settings", ','), ("Memory", 'm'), ("Data Explorer", 'd')] {
            let entry = entries
                .iter()
                .find(|e| e.label == label)
                .unwrap_or_else(|| panic!("{label} is not in the catalogue at all"));
            let row = &tool_rows(std::slice::from_ref(entry))[0];
            assert_eq!(
                row.trailing,
                format!("Alt+{key}"),
                "the sidebar does not offer {label} a working chord; `n/a` here means \
                 the page is unreachable from the panel that names it"
            );
        }
    }

    /// **A16.** `tool_rows` puts the real binding in the key column: `Alt+<key>`,
    /// `/` for Search, `n/a` where a tool cannot launch.
    ///
    /// The negative half — `n/a` rather than a key — is the one that matters:
    /// a tool that cannot run showing a chord teaches the user a key that does
    /// nothing, which is worse than a blank cell because they will try it.
    /// Synthetic entries, so the unavailable arm is reachable on a machine
    /// where everything happens to be installed.
    #[test]
    fn the_key_column_carries_the_binding_and_marks_the_unavailable() {
        let entries = vec![
            crate::usertools::Entry {
                tool: crate::usertools::Tool::Shell,
                label: "Shell".into(),
                key: 's',
                detail: "open a shell here".into(),
                available: true,
            },
            crate::usertools::Entry {
                tool: crate::usertools::Tool::Search,
                label: "Search".into(),
                key: '/',
                detail: "the command menu".into(),
                available: true,
            },
            crate::usertools::Entry {
                tool: crate::usertools::Tool::DataExplorer,
                label: "Data Explorer".into(),
                key: 'd',
                detail: String::new(),
                available: false,
            },
        ];
        let rows = tool_rows(&entries);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].name, "Shell");
        assert_eq!(rows[0].trailing, "Alt+s", "a launching tool lost its chord");
        assert_eq!(
            rows[1].trailing, "/",
            "Search is the command menu the character already opens"
        );
        assert_eq!(
            rows[2].trailing, "n/a",
            "an unavailable tool showed a key that would do nothing"
        );

        // …and they reach the drawn TOOLS section. A mapping nothing renders is
        // a mapping that can be right while the panel is wrong.
        let mut app = App::new((120, 30));
        app.set_tools(rows);
        let (drawn, _) = cells(&mut app, &view(), 120, 30);
        let all = drawn.join("\n");
        assert!(all.contains("Shell"), "{all}");
        assert!(all.contains("Alt+s"), "{all}");
    }

    /// **A18.** The chord *class* is discoverable where the design says keys
    /// are discovered — the QUICK HELP table — and it is a class the decoder
    /// honours.
    ///
    /// Weak, and named as weak in the checklist: it asserts that `Alt+key`
    /// appears and nothing about the other ten rows. The strong version is the
    /// one `tui-fork-inventory.md` §11 asks for — every key the table
    /// advertises resolving to a non-`Ignore` action — which needs the private
    /// `keymap()` or a parse of rendered cells, and is written down in the
    /// report rather than faked here.
    #[test]
    fn quick_help_names_the_tool_chords_and_the_decoder_honours_them() {
        use super::super::input::tool_key;
        use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let mut app = App::new((130, 40));
        app.set_tools(one_tool());
        let (rows, _) = cells(&mut app, &view(), 130, 40);
        assert!(
            rows.iter().any(|r| r.contains("Alt+key")),
            "the tool chords are not in QUICK HELP, so the class is undiscoverable: {rows:?}"
        );
        assert!(
            tool_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::ALT), false).is_some(),
            "QUICK HELP advertises the `Alt+key` class and the decoder claims no `Alt` key"
        );
    }

    // -----------------------------------------------------------------------
    // A11 / A12 / A13 — the pages' disclosures, with their negative controls
    // -----------------------------------------------------------------------

    /// **A11.** Every page names the key that gets out of it, on the page
    /// itself — and the conversation does not.
    ///
    /// There is no other way off a page: no scroll, no close affordance, no
    /// click target. The recorded shape of that complaint here is somebody
    /// pressing the chord again and reporting that it *"does not close"*.
    ///
    /// **The negative control is the load-bearing half.** A test that only
    /// checks the sentence appears passes over a page that never draws it and
    /// a frame that always does — which is exactly what hoisting the sentence
    /// into shared chrome looks like. So both halves read only the *main pane's
    /// rectangle*: `Esc` also lives in the sidebar's QUICK HELP as
    /// `close menu / leave page`, which is a different claim in different
    /// words, and a whole-screen `contains` is satisfied by it.
    ///
    /// Coupled to the word `Esc`, and nothing more. Expect to adjust on arrival
    /// only if the incoming pages leave by a different key — in which case the
    /// failure is the question, not the answer.
    #[test]
    fn every_page_names_the_key_that_leaves_it_and_the_conversation_does_not() {
        let v = view();
        for (name, pane) in [
            ("Settings", Pane::Page(Page::Settings)),
            ("Data Explorer", Pane::Page(Page::DataExplorer)),
            ("Memory", Pane::Page(Page::Memory)),
        ] {
            let (rows, r) = page_screen(&v, pane, 120, 40);
            let body = pane_text(&rows, &r);
            assert!(
                body.contains("Esc"),
                "{name} does not say how to leave it, and nothing on the page does:\n{body}"
            );
        }

        let mut a = App::new((120, 40));
        a.set_tools(one_tool());
        let (rows, _) = cells(&mut a, &v, 120, 40);
        let body = pane_text(&rows, &layout_for(&v, 120, 40));
        assert!(
            !body.contains("Esc"),
            "the conversation's own pane names Esc, so the assertions above pass \
             without any page drawing anything:\n{body}"
        );
    }

    /// **A12.** Each page's disclosure belongs to that page alone.
    ///
    /// Memory and the Data Explorer are the **same function**, differing only
    /// in the footer handed in, so the failure actually available here is a
    /// footer landing on the wrong page. *"A reader who saw `no embedding
    /// index` under the Data Explorer would conclude the session store is an
    /// index that failed, which is a worse state than no sentence at all."*
    ///
    /// A matrix with negatives, because the positive half alone is what the
    /// three page tests already do and every one of them stays green if one
    /// sentence is copied onto all three.
    ///
    /// The markers are one distinctive noun per page rather than the sentence,
    /// so a rewrite that keeps the meaning keeps this green. **Expect to adjust
    /// on arrival if a page's subject changes** — a Memory page that grew a
    /// real embedding index would rightly fail the `embedding` negative on the
    /// other two.
    #[test]
    fn each_pages_disclosure_belongs_to_that_page_alone() {
        const SETTINGS: &str = "telemetry";
        const EXPLORER: &str = "query box";
        const MEMORY: &str = "embedding";

        let v = view();
        let body = |pane| {
            let (rows, r) = page_screen(&v, pane, 120, 40);
            pane_text(&rows, &r)
        };
        let settings = body(Pane::Page(Page::Settings));
        let explorer = body(Pane::Page(Page::DataExplorer));
        let memory = body(Pane::Page(Page::Memory));

        for (name, screen, mine, theirs) in [
            ("Settings", &settings, SETTINGS, [EXPLORER, MEMORY]),
            ("Data Explorer", &explorer, EXPLORER, [SETTINGS, MEMORY]),
            ("Memory", &memory, MEMORY, [SETTINGS, EXPLORER]),
        ] {
            assert!(
                screen.contains(mine),
                "{name} lost its own disclosure ({mine:?}):\n{screen}"
            );
            for other in theirs {
                assert!(
                    !screen.contains(other),
                    "{name} is carrying another page's disclosure ({other:?}), which \
                     tells the reader the wrong thing is missing:\n{screen}"
                );
            }
        }
    }

    /// **A13.** The Data Explorer names the four things it will not draw, one
    /// by one.
    ///
    /// *"`Not drawn` on its own is a shrug."* The phrase alone stays true if
    /// the list behind it is replaced with nothing, or with four different
    /// items. The query box is the one a user will spend a minute hunting for,
    /// because every data explorer they have used has one.
    #[test]
    fn the_explorer_names_the_query_box_among_what_it_will_not_draw() {
        let v = view();
        let (rows, r) = page_screen(&v, Pane::Page(Page::DataExplorer), 120, 40);
        let body = pane_text(&rows, &r);
        for absent in ["query box", "columns", "elapsed", "chart"] {
            assert!(
                body.contains(absent),
                "the page does not say the {absent} is missing, so its absence reads \
                 as an oversight rather than a refusal:\n{body}"
            );
        }
    }

    // -----------------------------------------------------------------------
    // A14 — Settings shows this run, not a mockup
    // -----------------------------------------------------------------------

    /// **A14.** Settings shows *this run's* values beside where each came from,
    /// and an absent value reads as absent rather than as `0`.
    ///
    /// The page's subtitle is a promise — "what this run resolved, and where
    /// each value came from" — and nothing checked either half: *"a one-line
    /// change swapping `st.model` for a literal, or dropping the third column,
    /// left every existing assertion green, because they all key off the
    /// `Not wired yet` block at the bottom."*
    ///
    /// `notes/design/settings-wiring.md` makes "an absent value renders as
    /// absent, never as a plausible placeholder" an acceptance criterion for
    /// the whole Settings port, and the incoming page compiles the mockup's
    /// sample values into its paint path. So this is written **differentially**:
    /// the same page is rendered for two different runs, and each run's values
    /// must appear on its own screen and *not* on the other's. A row that is a
    /// constant — a compiled-in sample value, a literal where `st.model` was —
    /// is identical on both screens and fails here. A single-run `contains`
    /// cannot see that at all.
    ///
    /// Each assertion is on **one row** holding both the value and its
    /// provenance, which is the pairing the page promises; a `contains` over
    /// the screen is satisfied by the status bar with the Settings row blank.
    #[test]
    fn settings_shows_this_runs_values_beside_where_each_came_from() {
        let mut a = view();
        a.status.model = "a-model-only-run-a-has".into();
        a.status.session = "C:/sessions/only-run-a.jsonl".into();
        a.status.context = Some((0, 120_000));
        // Unset on purpose: the branch that has to say so rather than say zero.
        a.status.spend = None;

        let mut b = view();
        b.status.model = "a-model-only-run-b-has".into();
        b.status.session = "C:/sessions/only-run-b.jsonl".into();
        b.status.context = Some((0, 64_000));
        b.status.spend = Some((0, 900));

        let render = |v: &View| {
            let (rows, r) = page_screen(v, Pane::Page(Page::Settings), 140, 40);
            pane_rows(&rows, &r)
        };
        let a_rows = render(&a);
        let b_rows = render(&b);
        let shown = |rows: &[String]| rows.join("\n");

        for (what, mine, theirs, why) in [
            (
                "the model",
                "a-model-only-run-a-has",
                "a-model-only-run-b-has",
                "provider",
            ),
            (
                "the session log",
                "only-run-a.jsonl",
                "only-run-b.jsonl",
                "record of this run",
            ),
            (
                "the context limit",
                "120000",
                "64000",
                "compaction happens at this size",
            ),
        ] {
            assert!(
                a_rows.iter().any(|r| r.contains(mine) && r.contains(why)),
                "{what} is not shown beside where it came from — one row has to carry \
                 both, or the page's subtitle is not true:\n{}",
                shown(&a_rows)
            );
            assert!(
                !a_rows.iter().any(|r| r.contains(theirs)),
                "{what} shows a value belonging to a different run: the row is a \
                 constant, not this run's:\n{}",
                shown(&a_rows)
            );
            assert!(
                b_rows.iter().any(|r| r.contains(theirs)),
                "{what} did not change when the run did, so the page is drawing a \
                 compiled-in value rather than what this run resolved:\n{}",
                shown(&b_rows)
            );
        }

        assert!(
            a_rows
                .iter()
                .any(|r| r.contains("Per-goal budget") && r.contains("not set")),
            "an unset budget did not read as unset; a number here reads as a \
             configured limit of that size:\n{}",
            shown(&a_rows)
        );
        assert!(
            b_rows
                .iter()
                .any(|r| r.contains("Per-goal budget") && r.contains("900")),
            "a budget that is set did not show its figure, so `not set` above proves \
             nothing about the branch:\n{}",
            shown(&b_rows)
        );
    }

    // -----------------------------------------------------------------------
    // A1 — a page that ran out of room says so
    // -----------------------------------------------------------------------

    /// **A1.** A page that ran out of room says how many lines are missing and
    /// names the remedy, on its last row.
    ///
    /// *"At 80×24 — an ordinary default — the Settings page stopped after
    /// `PgUp/PgDn scroll` and the whole 'Not wired yet' block was off-screen,
    /// unreachable by any key, with nothing saying it existed."* The three page
    /// tests all render at 120×40, the smallest size at which the disclosure
    /// fits; an adversarial reviewer rendered one at 80×24 and the assertions
    /// were simply false there. These pages do not scroll, so a taller window
    /// is the only remedy that exists.
    ///
    /// **The 120×40 positive control is load-bearing**: without it a notice
    /// that fired at every size passes the assertions above for the wrong
    /// reason, and would itself be the noise the page exists to avoid.
    ///
    /// **The guarantee is weaker than the checklist's wording, measured here
    /// on 2026-08-26.** A1 says the notice "names the remedy"; at 80 columns —
    /// the width the whole item is about — the notice is 86 columns long and
    /// `fit_spans` cuts it at `…make the w…`, so the remedy is off the right
    /// edge on exactly the window that triggers it. What survives at 80 is the
    /// count and "this page does not scroll", which is the loss and the reason,
    /// not the way out. So the remedy is asserted at a width where it fits and
    /// the shortfall is written down rather than asserted away. Shortening the
    /// notice, or wrapping it onto two rows, would close it.
    #[test]
    fn a_page_that_runs_out_of_room_says_how_much_is_missing() {
        let v = view();
        for (name, pane) in [
            ("Settings", Pane::Page(Page::Settings)),
            ("Data Explorer", Pane::Page(Page::DataExplorer)),
        ] {
            let (rows, r) = page_screen(&v, pane, 80, 24);
            let body = pane_rows(&rows, &r);
            let last = body.last().cloned().unwrap_or_default();
            assert!(
                body.iter().any(|l| l.contains("more line")),
                "{name} at 80x24 dropped content and said nothing:\n{}",
                body.join("\n")
            );
            assert!(
                last.contains("more line"),
                "the notice is not on the last row the page has, where a reader who \
                 has run out of page is looking:\n{}",
                body.join("\n")
            );
            assert!(
                last.chars().any(|c| c.is_ascii_digit()),
                "the notice does not say how much is missing; \"something was cut\" \
                 leaves the reader unable to judge whether to resize:\n{last}"
            );
            assert!(
                last.contains("does not scroll"),
                "the notice does not say why the rest is unreachable, so a reader \
                 hunts for a scroll key that is not there:\n{last}"
            );

            // Wide enough for the whole notice: the remedy has to be in it.
            // 140 columns, not 120 — measured 2026-08-26: a 120-column window
            // leaves the chat pane 84 columns and the notice is 85, so it is
            // cut at `make the window tall…`. The remedy is only ever read by
            // somebody whose window is already wide.
            let (rows, r) = page_screen(&v, pane, 140, 24);
            let body = pane_rows(&rows, &r);
            let last = body.last().cloned().unwrap_or_default();
            assert!(
                last.contains("more line"),
                "{name} at 140x24 dropped content and said nothing:\n{}",
                body.join("\n")
            );
            assert!(
                last.contains("taller"),
                "the notice names the loss and not the way out of it; a taller window \
                 is the only remedy that exists, because these pages do not \
                 scroll:\n{last}"
            );
        }

        let (rows, r) = page_screen(&v, Pane::Page(Page::Settings), 120, 40);
        let body = pane_text(&rows, &r);
        assert!(
            !body.contains("more line"),
            "a page that fitted claimed it had been cut:\n{body}"
        );
        assert!(
            body.contains("telemetry"),
            "the disclosure that must survive at this size did not:\n{body}"
        );
    }

    // -----------------------------------------------------------------------
    // A5 / A6 — the sidebar latch and the click that sets it
    // -----------------------------------------------------------------------

    /// **A5.** The user's choice beats the width, in both directions.
    ///
    /// Automatic collapse below `AUTO_COLLAPSE_COLS` is right until somebody
    /// has an opinion. A user-expanded sidebar below the threshold is honoured;
    /// a user-collapsed one stays collapsed when the window widens. Same shape
    /// as `frame.rs`'s `pinned`: one bool for the posture, one for whose it is.
    /// Anything that sets `collapsed` without `by_user` reintroduces the
    /// automatic override.
    #[test]
    fn the_sidebar_latch_honours_the_user_over_the_width() {
        let auto = Latch::default();
        assert!(hidden(80, auto), "the automatic rule stopped collapsing");
        assert!(
            !hidden(120, auto),
            "the automatic rule collapses at any width"
        );
        let user_closed = Latch {
            collapsed: true,
            by_user: true,
        };
        assert!(
            hidden(200, user_closed),
            "a wide window reopened what the user closed"
        );
        let user_open = Latch {
            collapsed: false,
            by_user: true,
        };
        assert!(
            !hidden(80, user_open),
            "the width rule overruled a user's expand below the threshold"
        );

        // Through the toggle, which is the only thing that may set `by_user`.
        let v = view();
        let mut app = App::new((80, 24));
        let (rows, _) = cells(&mut app, &v, 80, 24);
        assert!(
            !rows.iter().any(|r| r.contains("SESSIONS")),
            "precondition: 80 columns should auto-collapse:\n{}",
            rows.join("\n")
        );
        app.toggle_sidebar(80);
        let (rows, _) = cells(&mut app, &v, 80, 24);
        assert!(
            rows.iter().any(|r| r.contains("SESSIONS")),
            "the user's expand was overruled by the width rule:\n{}",
            rows.join("\n")
        );
        app.toggle_sidebar(80);
        let (rows, _) = cells(&mut app, &v, 80, 24);
        assert!(
            !rows.iter().any(|r| r.contains("SESSIONS")),
            "the second toggle did not close it again:\n{}",
            rows.join("\n")
        );
    }

    /// **A6.** A left click on the SESSIONS header row toggles the sidebar;
    /// nothing else does; a collapsed sidebar takes no click.
    ///
    /// The collapse defect was reported as *"+ and - does not seem to work"* —
    /// **the owner aimed at the affordance with the mouse**, and nothing routed
    /// a click anywhere. The whole row is the target rather than the
    /// affordance's three cells: those columns are the sidebar's internal
    /// layout, and a three-cell target at a guessed offset is a miss magnet.
    ///
    /// The coordinates come from `regions` rather than being written down,
    /// because a mouse aims at what was on screen at the last paint. A
    /// hit-test against a freshly computed rectangle is a different rectangle,
    /// and that is the failure this arrangement exists to prevent.
    #[test]
    fn only_the_sessions_header_row_takes_a_click_and_it_latches() {
        let v = view();
        let mut app = App::new((120, 30));
        let (rows, _) = cells(&mut app, &v, 120, 30);
        let r = layout_for(&v, 120, 30);
        assert!(
            rows.iter().any(|r| r.contains("SESSIONS")),
            "precondition: the sidebar is not even drawn:\n{}",
            rows.join("\n")
        );

        // Everything that is not the header row.
        for (x, y, what) in [
            (r.sidebar.x + 2, r.sidebar.y, "the sidebar's top border row"),
            (r.sidebar.x + 2, r.sidebar.y + 2, "the first session row"),
            (r.main.x + 5, r.sidebar.y + 1, "the main pane, same row"),
            (
                r.sidebar.x - 1,
                r.sidebar.y + 1,
                "the float column left of it",
            ),
        ] {
            assert!(
                !app.click(x, y, 120),
                "{what} toggled the sidebar; the one control gets the one row"
            );
        }

        let header = r.sidebar.y + 1;
        assert!(
            app.click(r.sidebar.x + 2, header, 120),
            "the header click did not toggle"
        );
        let (rows, _) = cells(&mut app, &v, 120, 30);
        assert!(
            !rows.iter().any(|r| r.contains("SESSIONS")),
            "the sidebar is still drawn after a collapse click:\n{}",
            rows.join("\n")
        );

        // A user's click is a user's latch: widening must not reopen it.
        let (rows, _) = cells(&mut app, &v, 200, 40);
        assert!(
            !rows.iter().any(|r| r.contains("SESSIONS")),
            "the automatic rule overrode the user's click:\n{}",
            rows.join("\n")
        );
        assert!(
            !app.click(r.sidebar.x + 2, header, 200),
            "a collapsed sidebar has no cells on screen and took a click anyway"
        );
    }

    // -----------------------------------------------------------------------
    // A7 / A8 — the wrap-width seam
    // -----------------------------------------------------------------------

    /// **A7.** The transcript's wrap width *is* `chat::message_width` of the
    /// chat pane's own rectangle — exactly, in both directions.
    ///
    /// *"The seam that would fail silently.* The transcript is wrapped by this
    /// shell and painted by the chat pane at a gutter offset; if the two widths
    /// are derived separately, every wrapped row is clipped by the gutter's
    /// width and no test in either file notices."
    ///
    /// Asserted through the retained transcript's own height rather than
    /// through the private field, so it survives a rename and still pins the
    /// number both ways: a line of exactly `message_width` columns must occupy
    /// one row, and a line one column longer must occupy two. Too wide and the
    /// first assertion is unaffected while the second fails; too narrow and the
    /// first fails. A single-sided check passes for a wrap width that is merely
    /// *not larger*.
    #[test]
    fn the_wrap_width_is_the_chat_panes_message_width_exactly() {
        let v = view();
        let sk = skin();
        let r = layout_for(&v, 120, 30);
        let mw = chat::message_width(r.chat.width);
        assert!(
            mw < r.chat.width,
            "at 120 columns the pane affords a gutter, so the two must differ — \
             without this the assertions below cannot tell them apart"
        );

        let rows_for = |len: usize| {
            let mut app = App::new((120, 30));
            let _ = cells(&mut app, &v, 120, 30);
            app.push_block(vec![Line::raw("x".repeat(len))], &sk);
            let _ = cells(&mut app, &v, 120, 30);
            app.transcript.height(&sk)
        };
        let one = rows_for(1);
        assert_eq!(
            rows_for(usize::from(mw)),
            one,
            "a line of exactly the pane's message width wrapped, so the transcript is \
             wrapped narrower than the column it is painted into"
        );
        assert_eq!(
            rows_for(usize::from(mw) + 1),
            one + 1,
            "a line one column past the pane's message width did not wrap, so the \
             transcript is wrapped wider than the column it is painted into and every \
             wrapped row is clipped by the gutter"
        );
    }

    /// **A8.** A resize rewraps the retained transcript.
    ///
    /// Narrowing the window without a rewrap leaves every retained entry
    /// clipped at the old column — and nothing on screen says so.
    #[test]
    fn a_resize_rewraps_the_retained_transcript() {
        let v = view();
        let sk = skin();
        let mut app = App::new((100, 30));
        let _ = cells(&mut app, &v, 100, 30);
        app.push_block(vec![Line::raw("x".repeat(90))], &sk);
        let before = app.transcript.height(&sk);
        let _ = cells(&mut app, &v, 40, 20);
        assert!(
            app.transcript.height(&sk) > before,
            "narrowing the window did not rewrap the transcript: {before} rows before, \
             {} after",
            app.transcript.height(&sk)
        );
    }

    // -----------------------------------------------------------------------
    // A3 / A4 — the arithmetic that must not panic
    // -----------------------------------------------------------------------

    /// **A3.** `dock_height`'s ceiling never inverts its clamp, and never
    /// exceeds the room it was given.
    ///
    /// *"On a degenerate frame half-of-room is below the floors, and a clamp
    /// whose min exceeds its max is a **panic**, not a layout."* A resize can
    /// report anything mid-frame, so this sweeps rather than sampling.
    ///
    /// Both halves of the ceiling are pinned, because they fail in opposite
    /// directions: without `.min(room)` the dock is taller than the window it
    /// is in, and without `.max(5)` an approval panel shrinks below the five
    /// rows that hold a question and the keys to answer it.
    #[test]
    fn the_docks_ceiling_never_inverts_and_never_exceeds_the_room() {
        let mut plain = view();
        plain.menu = None;
        let mut menu = view();
        menu.menu = Some(crate::term::menu::MenuView {
            rows: vec![("help".into(), "about".into()); 4],
            selected: 0,
            note: None,
        });
        let mut prompt = view();
        prompt.prompt = Some(Prompt {
            title: "Approve Bash".into(),
            preview: (0..40).map(|i| format!("line {i}")).collect(),
            keys: vec![("y".into(), "yes".into())],
            question: "allow? ".into(),
        });

        for room in 0u16..=64 {
            for (what, v) in [("idle", &plain), ("menu", &menu), ("prompt", &prompt)] {
                let h = dock_height(v, room);
                assert!(
                    h <= room.max(1),
                    "the {what} dock wants {h} rows out of {room}: a dock taller than \
                     its window paints over the transcript it was capped to protect"
                );
            }
        }
        for room in 5u16..=64 {
            assert!(
                dock_height(&prompt, room) >= 5,
                "at {room} rows a pending question got fewer than five rows, which is \
                 the least that holds the question and the keys to answer it"
            );
        }
        assert_eq!(
            dock_height(&plain, 30),
            3,
            "the ordinary dock is the input box's three rows"
        );
        assert_eq!(
            dock_height(&prompt, 30),
            15,
            "the prompt is capped at half the pane"
        );
    }

    /// **A4.** `App::render` returns `None` on a zero-area window instead of
    /// laying out, and every degenerate size is survived.
    ///
    /// A resize can report `(0,0)`; ratatui's `Layout` on a zero rect and every
    /// `saturating_sub` below it are the difference between a redraw and a
    /// crash. The sweep is the original's, plus a page up — a page renderer
    /// walks rows against `end` and is the newer arithmetic.
    ///
    /// **This one has a survivor, measured 2026-08-26, and it is not new.**
    /// §6a's mutation for A4 is "delete the zero-area early return"; replacing
    /// the guard with `if false` leaves this test green — **and leaves `app.rs`'s
    /// own `degenerate_windows_are_survived` green too**, so the early return
    /// has never been defended by anything. Nothing downstream panics on a zero
    /// rect today, and `render_input` returns `None` for a zero dock anyway, so
    /// the guard has no observable effect through any public surface: what it
    /// buys is that the layout below it is never reached with a degenerate
    /// rect, which is insurance against arithmetic that has not been written
    /// yet. What this test does defend is the sweep itself — every mutation
    /// that makes the layout panic at a small size turns it red, which A3's two
    /// mutations both did.
    #[test]
    fn degenerate_windows_are_survived() {
        let v = view();
        let mut app = App::new((0, 0));
        for (w, h) in [(0u16, 0u16), (1, 1), (2, 2), (5, 3), (24, 8), (200, 2)] {
            let (_, cursor) = cells(&mut app, &v, w, h);
            if w == 0 || h == 0 {
                assert!(
                    cursor.is_none(),
                    "a zero-area window was laid out rather than refused"
                );
            }
        }
        for pane in [
            Pane::Page(Page::Settings),
            Pane::Page(Page::DataExplorer),
            Pane::Page(Page::Memory),
        ] {
            let mut app = App::new((0, 0));
            app.show_explorer("source  C:/store\nsessions  2 file(s)");
            app.show(pane);
            for (w, h) in [(0u16, 0u16), (1, 1), (5, 3), (24, 8), (200, 2)] {
                let _ = cells(&mut app, &v, w, h);
            }
        }
    }

    // -----------------------------------------------------------------------
    // A9 — the degradation ladder
    // -----------------------------------------------------------------------

    /// **A9.** The ladder, and the three gaps measured off the mockup itself.
    ///
    /// The mockup speaks for one window size (~128×41). *"Rows are the scarce
    /// axis: at 24 rows two of them are a whole approval key row."* The gaps
    /// were measured from `notes/design/mockup-tui.png` on 2026-08-13 — the
    /// design note's own sampled table recorded none of them, and the prose
    /// description of the image was wrong in five recorded places.
    ///
    /// The pane gap is **zero when the sidebar is collapsed**: it is the
    /// sidebar's, and goes with it, or it becomes a stray two-column stripe.
    #[test]
    fn the_degradation_ladder_and_the_mockups_gaps_are_in_the_carve() {
        let v = view();
        let r = regions(Rect::new(0, 0, 128, 41), sidebar::width(128, false), 3);
        assert_eq!(
            r.sidebar.x, 1,
            "the panes stopped floating inside the window: {r:?}"
        );
        assert_eq!(r.sidebar.y, 1, "{r:?}");
        assert_eq!(r.status.bottom(), 41 - 1, "{r:?}");
        assert_eq!(
            r.main.x - r.sidebar.right(),
            2,
            "the two columns of ground between the boxes are gone; they do not share \
             an edge in the image: {r:?}"
        );
        assert_eq!(
            r.status.y - r.main.bottom(),
            1,
            "the blank row between the pane's bottom border and the bar's top: {r:?}"
        );
        let collapsed = regions(Rect::new(0, 0, 128, 41), 0, 3);
        assert_eq!(
            collapsed.main.x, 1,
            "the pane gap outlived the sidebar and is now a stripe of nothing: {collapsed:?}"
        );

        // Rows first: the status border, then the header's subtitle and rule.
        let r = regions(Rect::new(0, 0, 100, 13), sidebar::width(100, false), 3);
        assert_eq!(r.status.height, 1, "under 14 rows the bar is one row");
        assert_eq!(r.header.height, 2, "13 rows still carries the subtitle");
        let r = regions(Rect::new(0, 0, 100, 11), sidebar::width(100, false), 3);
        assert_eq!(r.header.height, 1, "under 12 rows the header is one line");
        assert_eq!(r.rule.height, 0, "the rule outlived the subtitle");

        // Columns: the main border, and the inset that rides with it.
        let r = regions(Rect::new(0, 0, 59, 20), 0, 3);
        assert!(!r.main_bordered, "a 59-column window kept its border");
        assert_eq!(r.chat.width, 59, "an absent border costs no columns");

        // And the auto-collapse the plan's must-change #4 named.
        let mut app = App::new((80, 24));
        let (rows, cursor) = cells(&mut app, &v, 80, 24);
        assert!(
            !rows.iter().any(|r| r.contains("SESSIONS")),
            "at 80x24 the sidebar must be collapsed:\n{}",
            rows.join("\n")
        );
        assert!(cursor.is_some(), "nowhere to type at 80x24");
        assert!(
            rows.iter().any(|r| r.contains("Emma")),
            "the header did not survive 80x24"
        );
    }

    // -----------------------------------------------------------------------
    // A10 / A15 / A20 / A21 — the dock, and what a page may replace
    // -----------------------------------------------------------------------

    /// **A10.** The approval panel replaces the input box, and no amount of
    /// transcript output can move it.
    ///
    /// Design §4.6, and the defect the whole full-screen frame exists to fix,
    /// carried over from the inline viewport: a question scrolling off screen
    /// under tool output. The 200 blocks pushed first are the point — the
    /// question has to be on screen *after* them.
    #[test]
    fn a_pending_question_replaces_the_input_box_and_stays_on_screen() {
        let sk = skin();
        let mut app = App::new((100, 30));
        for i in 0..200 {
            app.push_block(vec![Line::raw(format!("output line {i}"))], &sk);
        }
        let mut v = view();
        v.prompt = Some(Prompt {
            title: "Approve Bash".into(),
            preview: vec!["$ rm -rf build/".into()],
            keys: vec![("y".into(), "yes".into()), ("n".into(), "no".into())],
            question: "allow? ".into(),
        });
        let (rows, cursor) = cells(&mut app, &v, 100, 30);
        let all = rows.join("\n");
        assert!(
            all.contains("Approve Bash"),
            "the question is not on screen:\n{all}"
        );
        assert!(
            all.contains("rm -rf build/"),
            "the evidence is not on screen:\n{all}"
        );
        assert!(
            all.contains(" y "),
            "the keys to answer it are not on screen:\n{all}"
        );
        assert!(
            !all.contains("> "),
            "the input box was drawn beside the question, so the dock has two \
             occupants and the answer can go to the wrong one:\n{all}"
        );
        assert!(cursor.is_some(), "there is nowhere to answer");
    }

    /// **A15.** The Memory page draws exactly one input box, and it is the
    /// conversation's, cell for cell.
    ///
    /// **This is a finding, not a feature.** The sidebar row and the mockup
    /// describe Memory as *"the only page with its own composer"*. There is no
    /// composer in this repository, and the thing this codebase would actually
    /// do wrong is draw a second box that looks like one and swallows nothing —
    /// the "declaration wearing the costume of a mechanism" it refuses
    /// everywhere else. The dock is drawn once, by `render`, after the pane
    /// match, for every pane; a page that draws its own is the defect.
    ///
    /// `[send: Enter]` is drawn once per input box, by the box itself, so
    /// counting it counts boxes. **Expect to adjust on arrival** if the input
    /// box's own hint changes wording — that string belongs to `view.rs`, which
    /// is not being replaced, but it is the one literal here that is not this
    /// file's.
    #[test]
    fn memory_draws_one_input_box_and_it_is_the_conversations() {
        let v = view();
        let mut chat_app = App::new((120, 40));
        chat_app.set_tools(one_tool());
        let (chat_rows, _) = cells(&mut chat_app, &v, 120, 40);

        let mut mem = App::new((120, 40));
        mem.set_tools(one_tool());
        mem.show_memory("project        C:/src/emma\ngoals asked    12");
        let (mem_rows, _) = cells(&mut mem, &v, 120, 40);

        let boxes = mem_rows
            .iter()
            .filter(|r| r.contains("[send: Enter]"))
            .count();
        assert_eq!(
            boxes,
            1,
            "the Memory page draws {boxes} input boxes; it has one, and it is the \
             conversation's:\n{}",
            mem_rows.join("\n")
        );

        let r = layout_for(&v, 120, 40);
        let dock = |rows: &[String]| {
            (r.dock.y..r.dock.bottom())
                .map(|y| rows[usize::from(y)].clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            dock(&mem_rows),
            dock(&chat_rows),
            "the Memory page's input dock is not the conversation's — a page-local \
             input is the composer this repository does not have"
        );

        // The same for the other two, which is A21's placement constraint seen
        // from the dock's side: the pane match runs *before* the dock, so a
        // page that painted outside its own rectangle would show up here as a
        // dock that differs from the conversation's.
        for (name, pane) in [
            ("Settings", Pane::Page(Page::Settings)),
            ("Data Explorer", Pane::Page(Page::DataExplorer)),
        ] {
            let (rows, _) = page_screen(&v, pane, 120, 40);
            assert_eq!(
                dock(&rows),
                dock(&chat_rows),
                "{name} changed the input dock; a page swaps the middle and leaves the \
                 chrome alone"
            );
        }
    }

    /// **A21, and A20's behavioural half.** A page swaps the middle and keeps
    /// the frame; leaving one loses no transcript.
    ///
    /// *"Every mockup for the tool pages keeps `SESSIONS`, `TOOLS`,
    /// `QUICK HELP` and the status row exactly as the chat view has them, and
    /// replaces the middle. So a page is not a window, not a mode, and not a
    /// second `App`."* Placement inside `render` is the item: sidebar and
    /// border before the pane match, dock and hint and status row after it — a
    /// page drawn last paints over the dock.
    ///
    /// `Pane::Chat` being a named variant rather than "no page" is A20, and its
    /// other half — that the match stays exhaustive, so a new page is a compile
    /// error at every site that decides what to draw — belongs to the compiler
    /// and cannot be asserted from here. A `_ =>` arm anywhere in that match
    /// deletes it with no test failing. Recorded, not faked.
    #[test]
    fn a_page_swaps_the_middle_and_keeps_the_frame_and_the_transcript() {
        let v = view();
        let sk = skin();
        let mut app = App::new((120, 40));
        app.set_tools(one_tool());
        assert_eq!(
            app.pane(),
            Pane::Chat,
            "the default occupant is not the conversation"
        );
        app.push_block(vec![Line::raw("a-line-only-the-transcript-has")], &sk);
        let (rows, cursor) = cells(&mut app, &v, 120, 40);
        assert!(
            cursor.is_some(),
            "the chat view swallowed the cursor position"
        );
        assert!(
            rows.join("\n").contains("a-line-only-the-transcript-has"),
            "precondition: the transcript is not drawn at all"
        );

        app.show(Pane::Page(Page::Settings));
        let (rows, cursor) = cells(&mut app, &v, 120, 40);
        let all = rows.join("\n");
        for (what, needle) in [
            ("the sidebar", "SESSIONS"),
            ("the tools panel", "TOOLS"),
            ("the status bar", "MODE"),
        ] {
            assert!(
                all.contains(needle),
                "{what} went with the page; the frame is the invariant:\n{all}"
            );
        }
        assert!(all.contains("Settings"), "the page did not draw:\n{all}");
        assert!(
            !all.contains("a-line-only-the-transcript-has"),
            "the transcript painted under the page:\n{all}"
        );
        assert!(
            cursor.is_some(),
            "a page took the dock's cursor, so there is nowhere to type on it"
        );

        app.show(Pane::Chat);
        let (rows, _) = cells(&mut app, &v, 120, 40);
        let all = rows.join("\n");
        assert!(
            all.contains("a-line-only-the-transcript-has"),
            "leaving the page lost the transcript; a page is a view, not a mode that \
             discards state:\n{all}"
        );
        assert!(
            !all.contains("telemetry"),
            "the page kept painting after it was left:\n{all}"
        );
    }

    // -----------------------------------------------------------------------
    // A22 / A23 / A25 — what this file draws itself
    // -----------------------------------------------------------------------

    /// **A22.** The header carries the wordmark, the real `CARGO_PKG_VERSION`,
    /// and the real cwd as the subtitle.
    ///
    /// Every word of it has to be true of *this* run — the mockup's serif
    /// wordmark and slogan are decoration a terminal cell cannot carry (design
    /// §3 Q8: one bold row, not figlet). The cwd is asserted against the value
    /// this run set rather than a substring of a path, so a hardcoded directory
    /// fails.
    #[test]
    fn the_header_carries_the_wordmark_the_version_and_this_runs_directory() {
        let mut v = view();
        v.status.cwd = "Q:\\a-directory-only-this-test-has".into();
        let mut app = App::new((100, 30));
        let (rows, _) = cells(&mut app, &v, 100, 30);
        let word = rows
            .iter()
            .find(|r| r.contains("Emma"))
            .expect("no wordmark on screen");
        assert!(
            word.contains(concat!("v", env!("CARGO_PKG_VERSION"))),
            "the header does not carry this build's version: {word:?}"
        );
        assert!(
            rows.iter()
                .any(|r| r.contains("a-directory-only-this-test-has")),
            "the subtitle is not this run's working directory:\n{}",
            rows.join("\n")
        );
    }

    /// **A23.** The hint row names the keys, and differs by mode — Working
    /// names the interrupt and that what you type now runs next.
    ///
    /// These are the keys nobody can guess. The scrolled-behind indicator is
    /// deliberately *not* here: the chat pane overlays it on its own last row,
    /// where the reader's eye already is, and this asserts that division as
    /// well as the wording.
    #[test]
    fn the_hint_names_the_keys_and_the_pane_owns_the_scroll_indicator() {
        let sk = skin();
        let mut app = App::new((100, 30));
        for i in 0..100 {
            app.push_block(vec![Line::raw(format!("line {i}"))], &sk);
        }
        let v = view();
        let r = layout_for(&v, 100, 30);
        let (rows, _) = cells(&mut app, &v, 100, 30);
        let hint = rows[usize::from(r.hint.y)].clone();
        assert!(
            hint.contains("Ctrl-B"),
            "the idle hint does not name the sidebar key: {hint:?}"
        );
        assert!(
            hint.contains("PgUp"),
            "the idle hint does not name the scroll keys: {hint:?}"
        );
        assert!(
            !rows.join("\n").contains("rows below"),
            "a following reader was told they are behind"
        );

        // Scrolled: the indicator appears, and on the pane rather than the hint.
        app.transcript.scroll_up(50);
        let (rows, _) = cells(&mut app, &v, 100, 30);
        let indicator = rows
            .iter()
            .find(|r| r.contains("rows below"))
            .expect("no catch-up indicator anywhere on screen")
            .clone();
        assert!(
            indicator.contains("End"),
            "the indicator does not name the way back: {indicator:?}"
        );
        assert!(
            !rows[usize::from(r.hint.y)].contains("rows below"),
            "the hint row took the pane's indicator; two places now claim the same fact"
        );

        // Working: a different row, naming the interrupt.
        let mut v = view();
        v.mode = Mode::Working;
        let (rows, _) = cells(&mut app, &v, 100, 30);
        let hint = rows[usize::from(r.hint.y)].clone();
        assert!(
            hint.contains("Ctrl-C") && hint.contains("runs next"),
            "the working hint does not name the interrupt and what a typed line does \
             now: {hint:?}"
        );
    }

    /// **A25.** `stem` returns the last non-empty path component, on either
    /// separator.
    ///
    /// The status bar's ENV cell. A trailing separator would otherwise yield an
    /// empty cell, which reads as a run with no directory.
    #[test]
    fn the_stem_is_the_last_real_component() {
        assert_eq!(stem("C:\\src\\emma"), "emma");
        assert_eq!(stem("/home/alan/emma/"), "emma");
        assert_eq!(stem("C:\\src\\emma\\"), "emma");
        assert_eq!(stem("emma"), "emma");
    }
}

// endregion: app.rs
