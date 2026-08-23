//! Every way out of the working directory that was thought of, tried.
//!
//! These are written as attempts rather than as unit tests of `resolve`,
//! because what matters is that the *tools* refuse — a containment check that
//! is correct and then not consulted protects nothing.

mod support;

use emma_tool_api::ToolError;
use serde_json::json;
use support::Sandbox;

// region: Paths that climb out, and the one that does not
// ---------------------------------------------------------------------------
// Paths that climb out, and the one that does not
//
// `..` leading and buried, and an absolute path elsewhere on the machine — the
// escapes that need no filesystem trickery. The last test in this section is
// the positive control, and it is not optional: rejecting every absolute path
// passes all three escapes above and breaks a call the model makes constantly.
// ---------------------------------------------------------------------------

/// Shared by every attempt below, so a refusal for the wrong reason cannot pass
/// for containment holding. A `Read` that refuses `../secret.txt` because the
/// file happens not to exist looks identical at the call site and protects
/// nothing, which is why the message is asserted and not only the outcome.
///
/// The class is checked for the same reason: `bad_arguments` says "name a
/// different path", which the model can act on, whereas `tool_failed` would say
/// the machinery is broken and send it looking for another route out.
fn assert_refused(error: ToolError, what: &str) {
    assert_eq!(
        error.kind(),
        "bad_arguments",
        "{what} was refused with the wrong class: {error}"
    );
    assert!(
        error.detail().contains("outside the working directory"),
        "{what} was refused for the wrong reason: {error}"
    );
}

/// Both directions, because they fail differently. Reading out is theft;
/// writing out is damage to a machine nobody said the agent could touch, and
/// the `Write` half is checked against a path that does not exist yet — the
/// case `canonicalize` alone cannot resolve and so cannot judge.
#[tokio::test]
async fn dot_dot_cannot_climb_out() {
    let sandbox = Sandbox::new();
    let outside = sandbox.root().parent().unwrap().join("outside-secret.txt");
    std::fs::write(&outside, "secret").unwrap();

    let error = sandbox
        .err("Read", json!({ "file_path": "../outside-secret.txt" }))
        .await;
    assert_refused(error, "../outside-secret.txt");

    let error = sandbox
        .err(
            "Write",
            json!({ "file_path": "../../pwned.txt", "content": "x" }),
        )
        .await;
    assert_refused(error, "../../pwned.txt");

    let _ = std::fs::remove_file(&outside);
}

#[tokio::test]
async fn dot_dot_buried_mid_path_cannot_climb_out() {
    // `a/b/../../../x` is the same escape wearing a disguise: it never starts
    // with `..` and a naive prefix check on the raw string lets it through.
    let sandbox = Sandbox::new();
    sandbox.write_file("a/b/keep.txt", "keep");
    let error = sandbox
        .err(
            "Read",
            json!({ "file_path": "a/b/../../../outside-secret.txt" }),
        )
        .await;
    assert_refused(error, "a/b/../../../outside-secret.txt");
}

/// The last assertion is the one that earns its keep: it checks the filesystem
/// rather than the error. A `Write` that creates the file and then returns a
/// refusal satisfies every message assertion here and has already done the
/// damage.
#[tokio::test]
async fn an_absolute_path_elsewhere_is_refused() {
    let sandbox = Sandbox::new();
    let elsewhere = if cfg!(windows) {
        "C:/Windows/win.ini"
    } else {
        "/etc/passwd"
    };
    let error = sandbox.err("Read", json!({ "file_path": elsewhere })).await;
    assert_refused(error, elsewhere);

    // Unique per run rather than a fixed name. The assertion below is "this
    // file does not exist", and it is read against a real, world-writable,
    // persistent directory — `/tmp` on unix survives reboots and is shared
    // between users. A fixed name means any leftover, from an earlier failure
    // or another checkout or somebody else entirely, fails this test with a
    // message accusing the tool of a write it never made.
    let target = format!(
        "{}emma-pwned-{}-{:x}.txt",
        if cfg!(windows) {
            "C:/Windows/Temp/"
        } else {
            "/tmp/"
        },
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0)
    );
    assert!(
        !std::path::Path::new(&target).exists(),
        "{target} existed before the write was even attempted"
    );
    let error = sandbox
        .err("Write", json!({ "file_path": target, "content": "x" }))
        .await;
    assert_refused(error, &target);
    assert!(
        !std::path::Path::new(&target).exists(),
        "the refused write created {target} anyway"
    );
}

#[tokio::test]
async fn an_absolute_path_inside_the_root_is_allowed() {
    // The containment check must not be "reject anything absolute" — that
    // passes every escape test above while breaking a legitimate call, which is
    // how a check ends up loosened later by someone who thinks it is wrong.
    let sandbox = Sandbox::new();
    sandbox.write_file("inside.txt", "hello\n");
    let absolute = sandbox.root().join("inside.txt");
    let outcome = sandbox
        .ok("Read", json!({ "file_path": absolute.to_string_lossy() }))
        .await;
    assert!(outcome.content.contains("hello"));
}

// endregion: Paths that climb out, and the one that does not

// region: Links that read and write through
// ---------------------------------------------------------------------------
// Links that read and write through
//
// A symlink inside the root pointing outward is an escape containing no `..`
// at all, which is why containment canonicalises rather than inspecting the
// string. Two surfaces have to hold it: the path argument, and the walk that
// `Glob` and `Grep` run. These skip rather than fail where the platform will
// not make a link, and say so — a silent `return` would look like a pass.
//
// The escape this section does NOT contain is a hard link, and that is stated
// in `path.rs` rather than tested, because there is nothing to detect: both
// names are equally the file and `canonicalize` returns the inside one.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_symlinked_directory_pointing_out_is_refused() {
    let sandbox = Sandbox::new();
    let outside_dir = sandbox.root().parent().unwrap().join("emma-outside-dir");
    std::fs::create_dir_all(&outside_dir).unwrap();
    std::fs::write(outside_dir.join("secret.txt"), "secret").unwrap();

    let link = sandbox.root().join("escape");
    if support::symlink_dir(&outside_dir, &link).is_none() {
        eprintln!("SKIPPED: this platform will not create a directory symlink");
        let _ = std::fs::remove_dir_all(&outside_dir);
        return;
    }

    // Reading through the link.
    let error = sandbox
        .err("Read", json!({ "file_path": "escape/secret.txt" }))
        .await;
    assert_refused(error, "escape/secret.txt");

    // And writing a file that does not exist yet through the link — the case
    // that cannot be canonicalised and so has to be caught by canonicalising
    // the parent.
    let error = sandbox
        .err(
            "Write",
            json!({ "file_path": "escape/planted.txt", "content": "x" }),
        )
        .await;
    assert_refused(error, "escape/planted.txt");
    assert!(
        !outside_dir.join("planted.txt").exists(),
        "the refused write planted a file outside the root"
    );

    let _ = std::fs::remove_dir_all(&outside_dir);
}

/// A file link rather than a directory link, because the two take different
/// code paths on Windows and a check that handles one is not evidence about the
/// other. There is no junction fallback here — Windows has no unprivileged
/// equivalent for files — so on a box without developer mode this reports a
/// skip, which is the honest outcome rather than a green tick.
#[tokio::test]
async fn a_symlinked_file_pointing_out_is_refused() {
    let sandbox = Sandbox::new();
    let outside = sandbox
        .root()
        .parent()
        .unwrap()
        .join("emma-outside-file.txt");
    std::fs::write(&outside, "secret").unwrap();

    let link = sandbox.root().join("escape.txt");
    if support::symlink_file(&outside, &link).is_none() {
        eprintln!("SKIPPED: this platform will not create a file symlink");
        let _ = std::fs::remove_file(&outside);
        return;
    }

    let error = sandbox
        .err("Read", json!({ "file_path": "escape.txt" }))
        .await;
    assert_refused(error, "escape.txt");

    let _ = std::fs::remove_file(&outside);
}

#[tokio::test]
async fn glob_and_grep_do_not_enumerate_through_a_symlink() {
    // Containment on the *argument* is not enough if the walk follows links:
    // a legal `Glob` of the root would otherwise list, and `Grep` would read,
    // files outside it.
    let sandbox = Sandbox::new();
    let outside_dir = sandbox.root().parent().unwrap().join("emma-outside-walk");
    std::fs::create_dir_all(&outside_dir).unwrap();
    std::fs::write(outside_dir.join("secret.txt"), "PASSWORD=hunter2").unwrap();

    let link = sandbox.root().join("escape");
    if support::symlink_dir(&outside_dir, &link).is_none() {
        eprintln!("SKIPPED: this platform will not create a directory symlink");
        let _ = std::fs::remove_dir_all(&outside_dir);
        return;
    }
    sandbox.write_file("inside.txt", "nothing here");

    let outcome = sandbox.ok("Glob", json!({ "pattern": "**/*.txt" })).await;
    assert!(
        !outcome.content.contains("secret.txt"),
        "Glob followed a symlink out of the root: {}",
        outcome.content
    );

    let outcome = sandbox.ok("Grep", json!({ "pattern": "hunter2" })).await;
    assert!(
        outcome.content.is_empty(),
        "Grep read through a symlink out of the root: {}",
        outcome.content
    );

    let _ = std::fs::remove_dir_all(&outside_dir);
}

// endregion: Links that read and write through

// region: The tools that take a path of their own
// ---------------------------------------------------------------------------
// The tools that take a path of their own
//
// `Glob`, `Grep` and `Bash` each accept a directory to start from, which is a
// second way in that does not go through a `file_path`. A containment check
// wired into `Read` and `Write` only would pass every test above and leave
// these three open.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn glob_and_grep_refuse_a_path_argument_outside_the_root() {
    let sandbox = Sandbox::new();
    let outside = sandbox
        .root()
        .parent()
        .unwrap()
        .to_string_lossy()
        .to_string();
    assert_refused(
        sandbox
            .err("Glob", json!({ "pattern": "*", "path": outside.clone() }))
            .await,
        "Glob path",
    );
    assert_refused(
        sandbox
            .err("Grep", json!({ "pattern": ".", "path": outside }))
            .await,
        "Grep path",
    );
}

#[tokio::test]
async fn bash_refuses_a_cwd_outside_the_root() {
    let sandbox = Sandbox::new();
    let outside = sandbox
        .root()
        .parent()
        .unwrap()
        .to_string_lossy()
        .to_string();
    assert_refused(
        sandbox
            .err("Bash", json!({ "command": "pwd", "cwd": outside }))
            .await,
        "Bash cwd",
    );
}

/// An empty path joins to the root itself, so without the explicit refusal
/// `Read ""` becomes `Read` of the working directory and the caller is told it
/// is a directory — a confusing answer to a call that was simply malformed.
#[tokio::test]
async fn an_empty_path_is_refused_rather_than_treated_as_the_root() {
    let sandbox = Sandbox::new();
    let error = sandbox.err("Read", json!({ "file_path": "" })).await;
    assert_eq!(error.kind(), "bad_arguments");
    assert!(error.detail().contains("empty"), "{error}");
}

// endregion: The tools that take a path of their own

// region: The re-check immediately before a write
// ---------------------------------------------------------------------------
// The re-check immediately before a write
//
// `DEF-016`: containment is checked against a *string* and the I/O happens
// later, so a directory component swapped in between lands the write outside
// the root. `path::still_contained` re-runs the check as the last thing before
// the write, which narrows the window and does not close it. These tests are
// about the guard, not about the race: a race that can be reproduced on demand
// is not a race, and asserting on one would be a receipt for something else.
// ---------------------------------------------------------------------------

/// Without this, the re-check can be deleted or turned into a tautology --
/// re-resolving the already-resolved path against itself always succeeds -- and
/// every other test in this file still passes.
///
/// The swap is done deliberately rather than raced: a directory inside the root
/// is replaced by a link pointing outside it, which is exactly the state the
/// window would leave behind, and the guard must refuse in that state.
#[tokio::test]
async fn a_directory_swapped_for_a_link_out_is_caught_by_the_recheck() {
    let sandbox = Sandbox::new();
    let root = sandbox.root().canonicalize().unwrap();
    let inside = root.join("sub");
    std::fs::create_dir_all(&inside).unwrap();

    // Resolved while `sub` is an ordinary directory.
    let resolved =
        emma_tools_fs::path::resolve(&root, "sub/file.txt").expect("an ordinary path resolves");

    // The window: `sub` becomes a link pointing out of the root.
    let outside = root.parent().unwrap().join("emma-outside-recheck");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::remove_dir(&inside).unwrap();
    if support::symlink_dir(&outside, &inside).is_none() {
        eprintln!("SKIPPED: this platform will not create a directory link");
        let _ = std::fs::remove_dir_all(&outside);
        return;
    }

    let err = emma_tools_fs::path::still_contained(&root, "sub/file.txt", &resolved)
        .expect_err("the re-check accepted a path that now leaves the root");
    let detail = format!("{err}");
    assert!(
        detail.contains("emma-outside-recheck") || detail.contains("outside"),
        "the refusal must name where it now goes: {detail}"
    );

    let _ = std::fs::remove_dir_all(&outside);
}

/// The positive control, and it is doing real work: the cheapest wrong way to
/// pass the test above is a re-check that refuses everything, and a `Write`
/// that always failed would be caught by nothing else in this file.
#[tokio::test]
async fn the_recheck_passes_an_ordinary_unchanged_path() {
    let sandbox = Sandbox::new();
    let root = sandbox.root().canonicalize().unwrap();
    std::fs::create_dir_all(root.join("sub")).unwrap();
    let resolved = emma_tools_fs::path::resolve(&root, "sub/file.txt").unwrap();
    emma_tools_fs::path::still_contained(&root, "sub/file.txt", &resolved)
        .expect("nothing moved, so nothing should be refused");

    // And through the tool, which is the path that actually matters.
    sandbox
        .ok(
            "Write",
            json!({ "file_path": "sub/file.txt", "content": "hello\n" }),
        )
        .await;
    assert_eq!(sandbox.read_file("sub/file.txt"), "hello\n");
}

/// The drift branch, which the escape test above does **not** cover.
///
/// When `sub` becomes a link pointing *out*, `resolve` itself refuses and the
/// comparison never runs — proved by mutation: disabling `again != resolved`
/// left that test green. So the branch needs its own case, and this is it: a
/// link to a different directory **inside** the root. Containment still holds,
/// the path resolved twice to two different places, and a write aimed at the
/// first would land in the second.
#[tokio::test]
async fn a_path_that_resolves_somewhere_else_inside_the_root_is_refused_too() {
    let sandbox = Sandbox::new();
    let root = sandbox.root().canonicalize().unwrap();
    let inside = root.join("sub");
    let elsewhere = root.join("elsewhere");
    std::fs::create_dir_all(&inside).unwrap();
    std::fs::create_dir_all(&elsewhere).unwrap();

    let resolved = emma_tools_fs::path::resolve(&root, "sub/file.txt").unwrap();

    std::fs::remove_dir(&inside).unwrap();
    if support::symlink_dir(&elsewhere, &inside).is_none() {
        eprintln!("SKIPPED: this platform will not create a directory link");
        return;
    }

    let err = emma_tools_fs::path::still_contained(&root, "sub/file.txt", &resolved)
        .expect_err("the re-check accepted a path that now names a different file");
    let detail = format!("{err}");
    assert!(
        detail.contains("moved underneath"),
        "the refusal must say the path drifted rather than blaming the argument: {detail}"
    );
}

// endregion: The re-check immediately before a write

// region: The hole, certified rather than asserted
// ---------------------------------------------------------------------------
// The hole, certified rather than asserted
//
// `path.rs`'s module doc has always said a hard link inside the root pointing
// at a file outside it is not detectable, and said it was verified rather than
// assumed. `docs/tools-containment.html` carried an amber chip disagreeing:
// the claim was repeated from the module doc, which does not say when or on
// what, and nothing in the suite exercised it. It named the experiment that
// would settle it. This is that experiment.
//
// It asserts the LIMITATION, not a fix. There is no fix available at this
// layer: both names are equally the file, there is no "real" path to
// canonicalise towards, and `canonicalize` returns the inside one. A test that
// pretended otherwise would be the false receipt this project keeps filing.
// What the test buys is that the limitation cannot quietly change — if a future
// resolver ever does catch this, this test goes red and somebody has to come
// and rewrite the doc rather than leaving it saying the opposite.
// ---------------------------------------------------------------------------

/// A hard link inside the root reads a file outside it, and that is documented.
///
/// **Read this as the boundary being a boundary against paths, not against an
/// operator who has already placed a link inside the tree.** `Bash` is a wider
/// hole than this and is documented as one; the approval gate is a consent
/// interface and not a sandbox. What would be indefensible is the claim going
/// unchecked, which is what the amber chip was for.
///
/// Certified on Windows, where `std::fs::hard_link` needs no privilege. The
/// skip is real and reported: a platform or filesystem that refuses a hard link
/// leaves nothing to certify, and pretending otherwise is worse than saying so.
#[tokio::test]
async fn a_hard_link_inside_the_root_reads_a_file_outside_it() {
    let sandbox = Sandbox::new();

    // **A name nothing else can collide with.** The first version of this used
    // `outside-secret.txt` in `root().parent()` -- which is the *shared* system
    // temp directory, and which `dot_dot_cannot_climb_out` above writes and
    // deletes under exactly that name. The two run in parallel by default, so
    // its `remove_file` landing between the write and the link here turned this
    // test into a silent skip, and the reverse order made it fail while blaming
    // the resolver. Found by an independent reviewer reading the file.
    let outside = sandbox.root().parent().unwrap().join(format!(
        "emma-hardlink-probe-{}-{}.txt",
        std::process::id(),
        line!()
    ));
    let _ = std::fs::remove_file(&outside);
    std::fs::write(&outside, "outside-secret\n").unwrap();

    let inside = sandbox.root().join("inside-link.txt");
    if std::fs::hard_link(&outside, &inside).is_err() {
        let _ = std::fs::remove_file(&outside);
        eprintln!("SKIPPED: this platform or filesystem would not create a hard link");
        return;
    }

    // **The control, and it is not optional.** Without it a `resolve` that
    // returned its argument unchecked would pass this test and still back the
    // certified chip: the test would never have shown that containment was
    // active in this sandbox at all. The same reviewer named that one-line
    // change. So: an ordinary climb out is refused here, in this sandbox, in
    // this run, before the interesting half is believed.
    let refused = sandbox
        .err("Read", json!({ "file_path": "../does-not-matter.txt" }))
        .await;
    assert_refused(refused, "../does-not-matter.txt");

    // `call` rather than `ok`, because `ok` panics on refusal with its own
    // message and would swallow the one below. If the hole ever closes, `Read`
    // refuses -- and this test has to be the thing that says so, not a generic
    // "failed unexpectedly".
    let out = sandbox
        .call("Read", json!({ "file_path": "inside-link.txt" }))
        .await;
    let _ = std::fs::remove_file(&outside);

    let content = match out {
        Ok(outcome) => outcome.content,
        Err(e) => panic!(
            "the hard-link hole appears to have closed: Read refused the link \
             rather than following it ({e}). That is good news and this test is \
             now wrong -- `tools/fs/src/path.rs`'s module doc and \
             `docs/tools-containment.html` both say the hole is open, and they \
             are what has to change."
        ),
    };
    assert!(
        content.contains("outside-secret"),
        "Read followed the link and returned something else, so this test no \
         longer demonstrates what it claims: {content:?}"
    );
}

// endregion: The hole, certified rather than asserted
