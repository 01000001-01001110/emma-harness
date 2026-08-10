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

use emma_llm::{
    AssistantTurn, Event, LlmError, Message, Mode, Provider, Request, ToolCall, Usage,
};
use emma_tool_api::{NetworkTarget, Tool, ToolCtx, ToolError, ToolMeta, ToolOutcome};
use serde_json::{json, Value};
use tokio::sync::mpsc;

// region: The model
// ---------------------------------------------------------------------------
// The model
//
// A `Provider` that answers from a script instead of a network, and records
// every request it was sent so a test can assert what the model was told.
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

    /// The last request only, where [`Fake::transcript`] is every request.
    ///
    /// The distinction is what makes an assertion specific: `transcript` proves
    /// the model was told something at some point in the run, and this proves
    /// it is in the message that was actually sent back on the final call —
    /// which is the claim behind the `raw_content` test.
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

    /// Every message the last request carried, history first, then query — the
    /// exact list that went on the wire.
    ///
    /// [`Fake::last_query`] renders the same thing to a string for a
    /// `contains` assertion. This one keeps the values, because the fold in
    /// `session.rs` claims to reproduce them and "some substring survived" is
    /// not that claim.
    pub fn last_messages(&self) -> Vec<Message> {
        let seen = self.seen.lock().unwrap();
        let last = seen.last().expect("the model was never called");
        last.history.iter().chain(last.query.iter()).cloned().collect()
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

// endregion: The model

// region: Tools
// ---------------------------------------------------------------------------
// Tools
//
// One tool with four knobs — its name, whether it claims `read_only`, which
// host it reaches if any, and whether it fails — plus a counter, because most
// assertions here are about whether the tool ran at all rather than what it
// returned.
// ---------------------------------------------------------------------------

pub struct TestTool {
    name: &'static str,
    read_only: bool,
    /// `Some` makes this a tool that declares egress and names its
    /// destination, which is the only shape the gate will grant.
    host: Option<&'static str>,
    fails: bool,
    calls: Arc<AtomicUsize>,
}

impl TestTool {
    pub fn ok(name: &'static str, read_only: bool) -> (Arc<dyn Tool>, Arc<AtomicUsize>) {
        Self::build(name, read_only, None, false)
    }

    pub fn failing(name: &'static str, read_only: bool) -> (Arc<dyn Tool>, Arc<AtomicUsize>) {
        Self::build(name, read_only, None, true)
    }

    /// A tool that changes nothing locally and reaches one host — the shape
    /// `WebFetch` and `WebSearch` have, and the shape that would run silently
    /// if the gate asked only about writing.
    pub fn reaching(name: &'static str, host: &'static str) -> (Arc<dyn Tool>, Arc<AtomicUsize>) {
        Self::build(name, true, Some(host), false)
    }

    fn build(
        name: &'static str,
        read_only: bool,
        host: Option<&'static str>,
        fails: bool,
    ) -> (Arc<dyn Tool>, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        (
            Arc::new(Self {
                name,
                read_only,
                host,
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
            reaches_network: self.host.is_some(),
            idempotent: true,
        }
    }

    fn network_target(&self, _args: &Value) -> Option<NetworkTarget> {
        self.host
            .map(|h| NetworkTarget::new(h, format!("reach {h} about something")))
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

// endregion: Tools

// region: A harness on disk
// ---------------------------------------------------------------------------
// A harness on disk
//
// Real `.emma/` directories in a temporary directory, loaded by the real
// `Harness`. The hook one dispatches a real process, which is why it has a
// platform split.
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

// endregion: A harness on disk
