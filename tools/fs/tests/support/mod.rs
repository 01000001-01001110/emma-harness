//! A sandbox the tests can be hostile to.
//!
//! Each integration test binary compiles the whole module and uses part of it,
//! so unused-warnings here are noise about the other binaries rather than about
//! anything unreachable.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use emma_tool_api::{Tool, ToolCtx, ToolError, ToolOutcome};
use emma_tools_fs::{fs_tools, ReadTracker};
use serde_json::Value;

pub struct Sandbox {
    pub dir: tempfile::TempDir,
    pub ctx: ToolCtx,
    pub tools: Vec<Arc<dyn Tool>>,
    pub tracker: Arc<ReadTracker>,
}

impl Sandbox {
    pub fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let (tools, tracker) = fs_tools();
        let ctx = ToolCtx {
            cwd: dir.path().to_path_buf(),
            session_id: "test-session".into(),
            turn_id: "turn-1".into(),
            background: Default::default(),
        };
        Self {
            dir,
            ctx,
            tools,
            tracker,
        }
    }

    pub fn root(&self) -> &Path {
        self.dir.path()
    }

    pub fn tool(&self, name: &str) -> &Arc<dyn Tool> {
        self.tools
            .iter()
            .find(|t| t.name() == name)
            .unwrap_or_else(|| panic!("no tool named {name}"))
    }

    pub fn write_file(&self, rel: &str, content: &str) -> PathBuf {
        let path = self.dir.path().join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("mkdir");
        }
        std::fs::write(&path, content).expect("write fixture");
        path
    }

    pub fn read_file(&self, rel: &str) -> String {
        std::fs::read_to_string(self.dir.path().join(rel)).expect("read fixture")
    }

    pub async fn call(&self, name: &str, args: Value) -> Result<ToolOutcome, ToolError> {
        self.tool(name)
            .invoke(&self.ctx, args)
            .await
            .expect("no turn-ending fault")
    }

    pub async fn ok(&self, name: &str, args: Value) -> ToolOutcome {
        match self.call(name, args).await {
            Ok(outcome) => outcome,
            Err(e) => panic!("{name} failed unexpectedly: {e}"),
        }
    }

    pub async fn err(&self, name: &str, args: Value) -> ToolError {
        match self.call(name, args).await {
            Ok(outcome) => panic!("{name} unexpectedly succeeded: {outcome:?}"),
            Err(e) => e,
        }
    }
}

/// A fingerprint of every file under a directory: relative path, length and
/// bytes. Used to prove a tool changed nothing at all — a weaker check (mtime,
/// or the top-level listing) would pass for a tool that rewrote a file with
/// content of the same length.
pub fn fingerprint(root: &Path) -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    collect(root, root, &mut out);
    out.sort();
    out
}

fn collect(root: &Path, dir: &Path, out: &mut Vec<(String, Vec<u8>)>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        let rel = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        if meta.is_dir() {
            out.push((format!("{rel}/"), Vec::new()));
            collect(root, &path, out);
        } else if meta.file_type().is_symlink() {
            out.push((format!("{rel}@"), Vec::new()));
        } else {
            out.push((rel, std::fs::read(&path).unwrap_or_default()));
        }
    }
}

/// Symlink creation, or `None` where the platform will not allow it.
///
/// Windows needs developer mode or an elevated process, so the symlink escape
/// tests report a skip rather than failing on a machine that simply cannot make
/// one. A silent `return` would have made the test look like it passed.
pub fn symlink_dir(target: &Path, link: &Path) -> Option<()> {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, link).ok()
    }
    #[cfg(windows)]
    {
        if std::os::windows::fs::symlink_dir(target, link).is_ok() {
            return Some(());
        }
        // A junction is the unprivileged equivalent and is still a reparse
        // point the OS follows, so the escape it enables is the same escape.
        // Without this fallback the symlink case silently never runs on a
        // Windows box that is not in developer mode — which is most of them.
        let status = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .ok()?;
        status.success().then_some(())
    }
}

pub fn symlink_file(target: &Path, link: &Path) -> Option<()> {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, link).ok()
    }
    #[cfg(windows)]
    {
        std::os::windows::fs::symlink_file(target, link).ok()
    }
}
