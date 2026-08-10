//! A model that does not exist, tools that do nothing, and a harness in a
//! temporary directory.
//!
//! The point of all of it is that the loop under test is the real one: the same
//! `Provider` trait, the same `Registry`, the same `Harness`, the same approval
//! gate. Nothing here is a re-implementation of a decision — the fakes supply
//! inputs and count calls, and every assertion is about what `Agent` did with
//! them.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use emma_llm::{AssistantTurn, Event, LlmError, Mode, Provider, Request, ToolCall, Usage};
use emma_tool_api::{Tool, ToolCtx, ToolError, ToolMeta, ToolOutcome};
use serde_json::{json, Value};
use tokio::sync::mpsc;

// ---------------------------------------------------------------------------
// The model
// ---------------------------------------------------------------------------

/// One scripted assistant turn.
pub struct Say {
    pub text: String,
    pub calls: Vec<(String, Value)>,
    pub tokens: i64,
}

pub fn text(t: &str) -> Say {
    Say {
        text: t.into(),
        calls: Vec::new(),
        tokens: 10,
    }
}

pub fn call(tool: &str, args: Value) -> Say {
    Say {
        text: String::new(),
        calls: vec![(tool.into(), args)],
        tokens: 10,
    }
}

impl Say {
    pub fn costing(mut self, tokens: i64) -> Self {
        self.tokens = tokens;
        self
    }
}

pub struct Fake {
    script: Mutex<std::collections::VecDeque<Say>>,
    seen: Mutex<Vec<Request>>,
}

impl Fake {
    pub fn new(script: Vec<Say>) -> Self {
        Self {
            script: Mutex::new(script.into()),
            seen: Mutex::new(Vec::new()),
        }
    }

    pub fn calls(&self) -> usize {
        self.seen.lock().unwrap().len()
    }

    /// Everything the model was ever sent, flattened. Tests assert on this to
    /// prove the model was *told* something — which is the whole claim behind
    /// "a failure reaches the model".
    pub fn transcript(&self) -> String {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .map(|r| {
                r.query
                    .iter()
                    .map(|m| m.content.to_string())
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The last request only — for asserting that something is *absent* from
    /// the most recent prompt rather than from the whole run.
    pub fn last_query(&self) -> String {
        self.seen
            .lock()
            .unwrap()
            .last()
            .map(|r| {
                r.query
                    .iter()
                    .map(|m| m.content.to_string())
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default()
    }
}

#[async_trait::async_trait]
impl Provider for Fake {
    fn model_id(&self) -> &str {
        "fake"
    }

    async fn send(
        &self,
        request: Request,
        _mode: Mode,
        _events: Option<mpsc::Sender<Event>>,
    ) -> Result<AssistantTurn, LlmError> {
        self.seen.lock().unwrap().push(request);
        let Some(say) = self.script.lock().unwrap().pop_front() else {
            // An unscripted call is a test that did something unexpected, and
            // must look like a failure rather than quietly ending the goal.
            return Err(LlmError::BadRequest {
                message: "the script ran out".into(),
            });
        };

        let mut blocks = Vec::new();
        if !say.text.is_empty() {
            blocks.push(json!({ "type": "text", "text": say.text }));
        }
        let mut tool_calls = Vec::new();
        for (i, (name, input)) in say.calls.iter().enumerate() {
            let id = format!("tu_{}_{i}", self.seen.lock().unwrap().len());
            blocks.push(json!({
                "type": "tool_use", "id": id, "name": name, "input": input
            }));
            tool_calls.push(ToolCall {
                id,
                name: name.clone(),
                input: input.clone(),
            });
        }
        Ok(AssistantTurn {
            text: say.text,
            stop_reason: if tool_calls.is_empty() {
                "end_turn".into()
            } else {
                "tool_use".into()
            },
            tool_calls,
            usage: Usage {
                input_tokens: say.tokens,
                output_tokens: 0,
                ..Default::default()
            },
            raw_content: Value::Array(blocks),
        })
    }
}

// ---------------------------------------------------------------------------
// Tools
// ---------------------------------------------------------------------------

pub struct TestTool {
    name: &'static str,
    read_only: bool,
    fails: bool,
    calls: Arc<AtomicUsize>,
}

impl TestTool {
    pub fn ok(name: &'static str, read_only: bool) -> (Arc<dyn Tool>, Arc<AtomicUsize>) {
        Self::build(name, read_only, false)
    }

    pub fn failing(name: &'static str, read_only: bool) -> (Arc<dyn Tool>, Arc<AtomicUsize>) {
        Self::build(name, read_only, true)
    }

    fn build(
        name: &'static str,
        read_only: bool,
        fails: bool,
    ) -> (Arc<dyn Tool>, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        (
            Arc::new(Self {
                name,
                read_only,
                fails,
                calls: calls.clone(),
            }),
            calls,
        )
    }
}

#[async_trait::async_trait]
impl Tool for TestTool {
    fn name(&self) -> &'static str {
        self.name
    }

    fn description(&self) -> &str {
        "a tool that exists for a test and does nothing else"
    }

    fn input_schema(&self) -> Value {
        json!({ "type": "object", "properties": { "x": { "type": "string" } } })
    }

    fn meta(&self) -> ToolMeta {
        ToolMeta {
            read_only: self.read_only,
            idempotent: true,
        }
    }

    async fn invoke(
        &self,
        _ctx: &ToolCtx,
        _args: Value,
    ) -> anyhow::Result<Result<ToolOutcome, ToolError>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(if self.fails {
            Err(ToolError::Failed(format!("{} broke on purpose", self.name)))
        } else {
            Ok(ToolOutcome::new(format!("{} ran", self.name)))
        })
    }
}

// ---------------------------------------------------------------------------
// A harness on disk
// ---------------------------------------------------------------------------

/// An empty `.emma/` — the boot state where Emma has no standing instructions.
pub fn empty_harness(dir: &Path) -> PathBuf {
    let root = dir.join(".emma");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("config.json"), "{}").unwrap();
    root
}

/// A `.emma/` with one `PreToolUse` hook that denies calls to `tool`.
///
/// The two platforms take different routes to the same verdict, and both are
/// real dispatches through `hooks.rs`:
///
/// - unix: a script that answers `{"decision":"deny", …}` — the deliberate
///   denial.
/// - windows: a program that exits non-zero, which `PreToolUse` resolves to
///   deny because it is fail-closed. Spawning a shell script needs a shell,
///   and `hooks.rs` execs the file directly with no `PATH` lookup and no
///   interpreter — by design, so there is nothing to test around.
pub fn harness_denying(dir: &Path, tool: &str) -> PathBuf {
    let root = dir.join(".emma");
    let hooks = root.join("hooks");
    std::fs::create_dir_all(&hooks).unwrap();

    let command = if cfg!(windows) {
        std::fs::copy("C:\\Windows\\System32\\where.exe", hooks.join("guard.exe")).unwrap();
        "hooks/guard.exe"
    } else {
        let path = hooks.join("guard.sh");
        std::fs::write(
            &path,
            "#!/bin/sh\necho '{\"decision\":\"deny\",\"reason\":\"policy forbids it\"}'\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        "hooks/guard.sh"
    };

    std::fs::write(
        root.join("config.json"),
        json!({
            "hooks": {
                "guard": { "event": "PreToolUse", "command": command, "matcher": tool }
            }
        })
        .to_string(),
    )
    .unwrap();
    root
}

pub fn registry(tools: Vec<Arc<dyn Tool>>) -> emma_tool_api::Registry {
    let mut r = emma_tool_api::Registry::new();
    for t in tools {
        r.register(t);
    }
    r
}
