//! Scratch directories and a fake tool registry, shared by the test binaries.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// A fresh directory per call. The counter is per-process and the pid separates
/// processes, so two test binaries running at once cannot collide.
pub fn scratch(tag: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("emma-{tag}-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

pub fn write(path: &Path, body: &str) {
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    std::fs::write(path, body).expect("write");
}

/// A `.emma/` with one persona whose `rules.md` is the given text.
pub fn one_persona(tag: &str, rules: &str) -> PathBuf {
    let root = scratch(tag).join(".emma");
    write(
        &root.join("config.json"),
        r#"{"default_persona":"assistant","personas":{"assistant":{}}}"#,
    );
    write(&root.join("personas/assistant/rules.md"), rules);
    root
}

pub fn with_skill(root: &Path, name: &str, description: &str, body: &str) {
    write(
        &root.join(format!("skills/{name}/SKILL.md")),
        &format!("---\nname: {name}\ndescription: {description}\n---\n\n{body}\n"),
    );
}

// ---------------------------------------------------------------------------
// A registry of tools that do nothing, for the allowlist tests
// ---------------------------------------------------------------------------

use emma_tool_api::{Registry, Tool, ToolCtx, ToolError, ToolMeta, ToolOutcome};
use std::sync::Arc;

pub struct Stub(pub &'static str);

#[async_trait::async_trait]
impl Tool for Stub {
    fn name(&self) -> &'static str {
        self.0
    }
    fn description(&self) -> &str {
        "a stub"
    }
    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({ "type": "object" })
    }
    fn meta(&self) -> ToolMeta {
        ToolMeta {
            read_only: true,
            idempotent: true,
        }
    }
    async fn invoke(
        &self,
        _ctx: &ToolCtx,
        _args: serde_json::Value,
    ) -> anyhow::Result<Result<ToolOutcome, ToolError>> {
        Ok(Ok(ToolOutcome::new("")))
    }
}

pub fn registry(names: &[&'static str]) -> Registry {
    let mut r = Registry::new();
    for n in names {
        r.register(Arc::new(Stub(n)));
    }
    r
}
