//! Every way out of the working directory that was thought of, tried.
//!
//! These are written as attempts rather than as unit tests of `resolve`,
//! because what matters is that the *tools* refuse — a containment check that
//! is correct and then not consulted protects nothing.

mod support;

use emma_tool_api::ToolError;
use serde_json::json;
use support::Sandbox;

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

    let target = if cfg!(windows) {
        "C:/Windows/Temp/emma-pwned.txt"
    } else {
        "/tmp/emma-pwned.txt"
    };
    let error = sandbox
        .err("Write", json!({ "file_path": target, "content": "x" }))
        .await;
    assert_refused(error, target);
    assert!(
        !std::path::Path::new(target).exists(),
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

#[tokio::test]
async fn an_empty_path_is_refused_rather_than_treated_as_the_root() {
    let sandbox = Sandbox::new();
    let error = sandbox.err("Read", json!({ "file_path": "" })).await;
    assert_eq!(error.kind(), "bad_arguments");
    assert!(error.detail().contains("empty"), "{error}");
}
