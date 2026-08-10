//! Scratch directories and a fake tool registry, shared by the test binaries.
//!
//! Everything the harness does is a function of a directory on disk, so every
//! test starts by building one. These helpers exist so a test reads as the
//! configuration it is about rather than as a page of `create_dir_all` — the
//! interesting line in a load test should be the JSON, not the scaffolding.
//!
//! Directories are left behind rather than cleaned up: a failing test is much
//! easier to diagnose with the tree it failed on still sitting in the temp
//! directory, and the names are unique enough that nothing accumulates on top of
//! anything else.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

// region: Scratch harnesses on disk
// ---------------------------------------------------------------------------
// Scratch harnesses on disk
//
// Building the directory a test is about. `one_persona` and `with_skill` cover
// the two shapes almost every test needs, so a test that builds its tree by
// hand is signalling that its tree is the interesting part.
// ---------------------------------------------------------------------------

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

// endregion: Scratch harnesses on disk

// region: A registry of tools that do nothing, for the allowlist tests
// ---------------------------------------------------------------------------
// A registry of tools that do nothing, for the allowlist tests
//
// `select_tools` takes a `Registry`, so the allowlist tests need one — but they
// are about which tools come out and in what order, never about what a tool
// does. `Stub` is the smallest thing that satisfies the trait.
// ---------------------------------------------------------------------------

use emma_tool_api::{Registry, Tool, ToolCtx, ToolError, ToolMeta, ToolOutcome};
use std::sync::Arc;

/// A tool that is nothing but a name. The allowlist tests are about which tools
/// survive `select_tools` and in what order, so behaviour would only be
/// something else that could break them; `invoke` is never called.
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
            reaches_network: false,
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

// endregion: A registry of tools that do nothing, for the allowlist tests
