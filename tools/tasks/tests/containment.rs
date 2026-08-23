//! The list lives under the working root, and there is exactly one check that
//! says so.
//!
//! The path is a constant, so the interesting case is not a traversal string —
//! there is nowhere to put one. It is `.emma` being a link that points out of
//! the tree, which is a real configuration people write, and which a naive
//! "does the string contain `..`" check would wave straight through.

mod support;

use emma_tool_api::ToolCtx;
use serde_json::json;
use support::Project;

/// The positive control. Without it, a containment bug that refused everything
/// would still pass the two tests below, and "the tool never writes anywhere"
/// is not the property being claimed.
#[tokio::test]
async fn the_file_resolves_inside_the_working_root() {
    let project = Project::new();
    project.ok("TaskCreate", json!({ "tasks": ["a"] })).await;
    let written = project.tasks_file().canonicalize().expect("written file");
    assert!(
        written.starts_with(project.root().canonicalize().unwrap()),
        "{written:?} escaped the root"
    );
}

#[tokio::test]
async fn an_emma_directory_linked_out_of_the_tree_is_refused() {
    let Some((project, outside)) = linked_emma() else {
        eprintln!("skipped: this platform will not create a directory link unprivileged");
        return;
    };

    let error = project.err("TaskCreate", json!({ "tasks": ["a"] })).await;
    assert_eq!(error.kind(), "bad_arguments");
    assert!(
        error.detail().contains("outside the working directory"),
        "{error}"
    );
    assert!(
        !outside.path().join("tasks/tasks.md").exists(),
        "the write escaped the root"
    );

    // The readers must refuse for the same reason rather than quietly
    // reporting an empty list, which would read as "no tasks" to the model.
    let error = project.err("TaskList", json!({})).await;
    assert_eq!(error.kind(), "bad_arguments");
}

#[tokio::test]
async fn an_unresolvable_working_directory_is_unavailable_not_bad_arguments() {
    // A root that is gone is the machinery being absent, not the caller being
    // wrong. The model routes differently on the two.
    let dir = tempfile::tempdir().unwrap();
    let ctx = ToolCtx {
        cwd: dir.path().join("no-such-subdirectory"),
        session_id: "s".into(),
        turn_id: "t".into(),
        background: Default::default(),
    };
    let error = emma_tools_tasks::task_tools()
        .iter()
        .find(|t| t.name() == "TaskList")
        .unwrap()
        .invoke(&ctx, json!({}))
        .await
        .expect("no turn-ending fault")
        .expect_err("a missing root must not read as an empty list");
    assert_eq!(error.kind(), "tool_unavailable");
}

/// A project whose `.emma` is a link to a directory outside it.
fn linked_emma() -> Option<(Project, tempfile::TempDir)> {
    let project = Project::new();
    let outside = tempfile::tempdir().ok()?;
    let link = project.root().join(".emma");
    #[cfg(unix)]
    let made = std::os::unix::fs::symlink(outside.path(), &link).is_ok();
    #[cfg(windows)]
    let made = std::os::windows::fs::symlink_dir(outside.path(), &link).is_ok() || {
        // A junction is the unprivileged equivalent and is the same reparse
        // point the OS follows. Without it this test silently never runs on a
        // Windows box that is not in developer mode, which is most of them.
        std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&link)
            .arg(outside.path())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    };
    made.then_some((project, outside))
}
