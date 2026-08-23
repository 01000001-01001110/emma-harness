//! A project directory the tests can be hostile to.
//!
//! Each integration test binary compiles the whole module and uses part of it,
//! so unused-warnings here are noise about the other binaries.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use emma_tool_api::{Tool, ToolCtx, ToolError, ToolOutcome};
use emma_tools_tasks::{task_tools, RELATIVE_PATH};
use serde_json::Value;

pub struct Project {
    pub dir: tempfile::TempDir,
    pub ctx: ToolCtx,
    pub tools: Vec<Arc<dyn Tool>>,
}

impl Project {
    pub fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let ctx = ToolCtx {
            cwd: dir.path().to_path_buf(),
            session_id: "test-session".into(),
            turn_id: "turn-1".into(),
            background: Default::default(),
        };
        Self {
            dir,
            ctx,
            tools: task_tools(),
        }
    }

    pub fn root(&self) -> &Path {
        self.dir.path()
    }

    pub fn tasks_file(&self) -> PathBuf {
        self.dir.path().join(RELATIVE_PATH)
    }

    /// Put a file on disk the way a person would have — by hand, with whatever
    /// shape they felt like.
    pub fn hand_write(&self, content: &str) {
        let path = self.tasks_file();
        std::fs::create_dir_all(path.parent().unwrap()).expect("mkdir");
        std::fs::write(&path, content).expect("write");
    }

    pub fn read_tasks(&self) -> String {
        std::fs::read_to_string(self.tasks_file()).expect("read tasks.md")
    }

    pub fn tool(&self, name: &str) -> &Arc<dyn Tool> {
        self.tools
            .iter()
            .find(|t| t.name() == name)
            .unwrap_or_else(|| panic!("no tool named {name}"))
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

/// Pull the `#abcd` handles out of a tool result, in order.
pub fn ids(outcome: &ToolOutcome) -> Vec<String> {
    outcome
        .content
        .split_whitespace()
        .filter_map(|w| w.strip_prefix('#'))
        .map(|s| s.to_string())
        .collect()
}

/// A fingerprint of every file under a directory. Used to prove a read-only
/// tool changed nothing at all — a weaker check would pass for a tool that
/// rewrote a file with content of the same length.
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
        } else {
            out.push((rel, std::fs::read(&path).unwrap_or_default()));
        }
    }
}
