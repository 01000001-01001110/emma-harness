//! A model that does not exist, tools that do nothing, and a harness in a
//! temporary directory.
//!
//! The point of all of it is that the loop under test is the real one: the same
//! `Provider` trait, the same `Registry`, the same `Harness`, the same approval
//! gate. Nothing here is a re-implementation of a decision — the fakes supply
//! inputs and count calls, and every assertion is about what `Agent` did with
//! them.

// A `mod support;` is compiled once into every test binary that declares it, so
// anything only `loop.rs` uses is dead code from `resume.rs`'s point of view and
// vice versa. The alternative is a support module per test file, which is two
// copies of the fake provider.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use emma_llm::{
    AssistantTurn, ContentBlock, Event, LlmError, Message, Mode, Provider, Request, ThinkingBlock,
    ToolCall, Usage,
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
    /// Answer this call with `LlmError::BadRequest` carrying this message,
    /// rather than with a turn.
    ///
    /// The provider rejecting a *request* — as opposed to failing to reach one
    /// — is a case the loop now has a recovery for: a model change can leave
    /// signed thinking blocks in the history that the new model will not take.
    /// A fake that could only succeed could not exercise it.
    pub fail: Option<String>,
}

pub fn text(t: &str) -> Say {
    Say {
        text: t.into(),
        calls: Vec::new(),
        tokens: 10,
        fail: None,
    }
}

pub fn call(tool: &str, args: Value) -> Say {
    Say {
        text: String::new(),
        calls: vec![(tool.into(), args)],
        tokens: 10,
        fail: None,
    }
}

/// A call the provider refuses as malformed.
pub fn rejected(message: &str) -> Say {
    Say {
        text: String::new(),
        calls: Vec::new(),
        tokens: 0,
        fail: Some(message.into()),
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
    /// What this client answers `model_id()` with. A session may now change
    /// model mid-conversation, and a test of that needs two clients that are
    /// distinguishable.
    id: String,
}

impl Fake {
    /// An `Arc`, because `Setup::provider` owns a counted handle rather than
    /// borrowing one — a session may now swap the model under a running agent,
    /// and a borrow could not express that.
    pub fn new(script: Vec<Say>) -> Arc<Self> {
        Self::named("fake", script)
    }

    pub fn named(id: &str, script: Vec<Say>) -> Arc<Self> {
        Arc::new(Self {
            script: Mutex::new(script.into()),
            seen: Mutex::new(Vec::new()),
            id: id.to_string(),
        })
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
        last.history
            .iter()
            .chain(last.query.iter())
            .cloned()
            .collect()
    }
}

#[async_trait::async_trait]
impl Provider for Fake {
    fn model_id(&self) -> &str {
        &self.id
    }

    async fn send(
        &self,
        request: Request,
        _mode: Mode,
        _events: Option<mpsc::Sender<Event>>,
    ) -> Result<AssistantTurn, LlmError> {
        self.seen.lock().unwrap().push(request);
        let script_head = self.script.lock().unwrap().pop_front();
        if let Some(message) = script_head.as_ref().and_then(|s| s.fail.clone()) {
            return Err(LlmError::BadRequest { message });
        }
        let Some(say) = script_head else {
            // An unscripted call is a test that did something unexpected, and
            // must look like a failure rather than quietly ending the goal.
            return Err(LlmError::BadRequest {
                message: "the script ran out".into(),
            });
        };

        // Blocks only, with `text` and the tool calls as views onto them —
        // the same shape the real provider produces. A fake that carried its
        // own copies of those two could disagree with its own content array,
        // which is precisely the drift the typed turn removed.
        let n = self.seen.lock().unwrap().len();
        let mut content = Vec::new();
        // A thinking block on every turn, signature and all, because it is the
        // one thing the loop must hand back untouched and a fake that never
        // produced one would let `the_assistant_turn_is_echoed_back_exactly_as_
        // it_arrived` assert about a turn with nothing fragile in it.
        content.push(ContentBlock::Thinking(ThinkingBlock {
            thinking: "fake thinking".into(),
            signature: Some(format!("sig-{n}")),
            ..Default::default()
        }));
        if !say.text.is_empty() {
            content.push(ContentBlock::text(say.text.clone()));
        }
        for (i, (name, input)) in say.calls.iter().enumerate() {
            content.push(ContentBlock::ToolUse(ToolCall {
                id: format!("tu_{n}_{i}"),
                name: name.clone(),
                input: input.clone(),
                // The real API puts a `caller` key on every `tool_use` block,
                // and a fake that never did would not exercise the path a live
                // run found broken.
                extra: serde_json::Map::from_iter([(
                    "caller".to_string(),
                    json!({ "type": "direct" }),
                )]),
            }));
        }
        Ok(AssistantTurn {
            stop_reason: if say.calls.is_empty() {
                "end_turn".into()
            } else {
                "tool_use".into()
            },
            content,
            usage: Usage {
                input_tokens: say.tokens,
                output_tokens: 0,
                ..Default::default()
            },
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
    /// What a successful call returns. `None` is `"<name> ran"`, which is
    /// enough for a test that only counts calls; a test asserting that a
    /// *result* survived into a later turn needs a body it can search for.
    body: Option<String>,
    calls: Arc<AtomicUsize>,
}

impl TestTool {
    pub fn ok(name: &'static str, read_only: bool) -> (Arc<dyn Tool>, Arc<AtomicUsize>) {
        Self::build(name, read_only, None, false, None)
    }

    pub fn failing(name: &'static str, read_only: bool) -> (Arc<dyn Tool>, Arc<AtomicUsize>) {
        Self::build(name, read_only, None, true, None)
    }

    /// A read-only tool whose output is a string the test can look for later.
    pub fn returning(
        name: &'static str,
        body: impl Into<String>,
    ) -> (Arc<dyn Tool>, Arc<AtomicUsize>) {
        Self::build(name, true, None, false, Some(body.into()))
    }

    /// A tool that changes nothing locally and reaches one host — the shape
    /// `WebFetch` and `WebSearch` have, and the shape that would run silently
    /// if the gate asked only about writing.
    pub fn reaching(name: &'static str, host: &'static str) -> (Arc<dyn Tool>, Arc<AtomicUsize>) {
        Self::build(name, true, Some(host), false, None)
    }

    fn build(
        name: &'static str,
        read_only: bool,
        host: Option<&'static str>,
        fails: bool,
        body: Option<String>,
    ) -> (Arc<dyn Tool>, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        (
            Arc::new(Self {
                name,
                read_only,
                host,
                fails,
                body,
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
            Ok(ToolOutcome::new(match &self.body {
                Some(body) => body.clone(),
                None => format!("{} ran", self.name),
            }))
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
/// Both arms answer `{"decision":"deny", …}` — the *deliberate* denial — and
/// that symmetry is deliberate too. The Windows arm used to copy `where.exe`
/// and rely on its non-zero exit, which resolves to deny by the fail-closed
/// rule. Same verdict, different mechanism: every test built on this helper was
/// then exercising the JSON-decision parser on unix only and the fail-closed
/// fallback on Windows only, so a regression in either was invisible to half
/// the world and no assertion changed to say so.
///
/// The script is a `.cmd` on Windows because `hooks.rs` execs the file directly
/// — no `PATH` lookup, no interpreter — and `.cmd` is the one extension the
/// standard library routes through a shell for us; a `#!` line means nothing
/// there. (The fail-closed path has its own coverage in
/// `crates/harness/tests/harness_hooks.rs`, on both platforms.)
pub fn harness_denying(dir: &Path, tool: &str) -> PathBuf {
    let root = dir.join(".emma");
    let hooks = root.join("hooks");
    std::fs::create_dir_all(&hooks).unwrap();

    let command = if cfg!(windows) {
        let path = hooks.join("guard.cmd");
        std::fs::write(
            &path,
            "@echo off\r\necho {\"decision\":\"deny\",\"reason\":\"policy forbids it\"}\r\n",
        )
        .unwrap();
        "hooks/guard.cmd"
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
