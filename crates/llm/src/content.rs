//! What a message is made of, typed.
//!
//! This module exists to delete one field. [`crate::AssistantTurn`] used to
//! carry `raw_content: serde_json::Value` — Anthropic's content array, handed
//! back on the next call without ever being read — and the loop built its own
//! `{"type":"tool_result",…}` objects to answer it. Between them those two
//! facts meant a second provider was a refactor of the *loop* rather than a
//! second file beside `anthropic.rs`, because the loop knew the wire shape.
//!
//! # Why a blob existed at all, and why it does not have to
//!
//! The blob was not laziness. Providers demand that some state be echoed back
//! **byte for byte** — this family rejects a thinking block whose signature has
//! been re-derived, so a turn rebuilt from `text` + `tool_calls` makes the
//! *next* call fail — and the cheapest way to guarantee that is to never look
//! inside it.
//!
//! `goose` (Block, Apache-2.0, `crates/goose-provider-types/src/conversation/
//! message.rs`) is the same conclusion reached the other way round, in
//! production Rust: opaque provider state gets a **narrow named field on an
//! otherwise fully typed block** — its `ThinkingContentBlock { thinking,
//! signature }` is [`ThinkingBlock`] here, and its `RedactedThinkingContentBlock
//! { data }` is [`RedactedThinkingBlock`]. Those two shapes are lifted from it
//! directly, as is its per-block `ProviderMetadata` map — `extra` here. See the
//! next section for why that last one is present against this module's own
//! first instinct.
//!
//! # The rule that makes this safe to type
//!
//! **Nothing is ever dropped, and the semantics are never given up to achieve
//! that.** Two mechanisms, and the split between them was decided by a live run
//! rather than by taste:
//!
//! - A block whose `type` this client does not know travels whole, as
//!   [`ContentBlock::Passthrough`]. There is nothing to interpret, so there is
//!   nothing lost by not interpreting it.
//! - A block whose `type` *is* known but which carries fields this client does
//!   not model keeps its variant, and the unmodelled keys go in that block's
//!   [`extra`](ToolCall::extra) map, which is echoed back untouched.
//!
//! The second mechanism is goose's `ProviderMetadata` under another name, and
//! this module was first written without it, on the argument that a per-block
//! bag recreates `raw_content` at a smaller scale. **That argument was wrong,
//! and one live call proved it.** The Messages API returns every `tool_use`
//! block with a `caller` key — observed on `claude-sonnet-5`, 2026-08-11, and
//! documented nowhere this client had read:
//!
//! ```text
//! {"caller":{"type":"direct"},"id":"toolu_01FJxk…","input":{…},"name":"Read","type":"tool_use"}
//! ```
//!
//! With strict decoding that block was not a tool call — it was an unrecognised
//! blob. The bytes round-tripped perfectly and the loop saw no tool calls, so a
//! real goal ended on the first turn reported as "answered — no tools were
//! needed". Preserving the bytes while losing the meaning is the worse of the
//! two failures: nothing is malformed, nothing errors, and the agent simply
//! stops working. So `extra` exists, and it is the narrowest thing that fixes
//! it.
//!
//! **What may live in `extra`, exhaustively:** keys the provider put on a block
//! whose `type` this client models, which this client does not read. It is
//! written only by [`ContentBlock::from_value`] and read only by
//! [`ContentBlock::to_value`]. Nothing branches on its contents, and a key that
//! Emma ever needs to *act* on is a field promoted out of it, not a lookup into
//! it.
//!
//! Neither mechanism is theoretical. `anthropic::Partial::Opaque` carries a
//! comment about the bug that produced it: a `_ => {}` match arm that dropped
//! every block type the client did not recognise, thinking blocks among them,
//! invisibly until a turn depended on one. A typed enum with an exhaustive
//! match is the same shape of mistake with the compiler's blessing, so the
//! fallbacks are structural rather than a habit.
//!
//! # What must not drift
//!
//! The bytes. Every block here serialises to the object the Messages API
//! documents, and `cache_control` is still attached in `anthropic.rs` to the
//! *rendered* JSON rather than to a field here — so where a breakpoint lands is
//! untouched by this module, by construction rather than by care.

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{json, Value};

// region: The blocks
// ---------------------------------------------------------------------------
// The blocks
//
// One struct per block type the Messages API defines and Emma can produce or
// consume, plus the one variant that admits this list is not the whole API.
// ---------------------------------------------------------------------------

/// Assistant prose, or a text block inside a user turn.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct TextBlock {
    pub text: String,
    /// Keys the provider put on this block that Emma does not model, echoed
    /// back untouched. See the module doc for what may live here and for the
    /// live call that made it necessary.
    #[serde(flatten, default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub extra: serde_json::Map<String, Value>,
}

/// A thinking block, and the opaque token that proves this client did not touch
/// it.
///
/// `signature` is the narrow named field this whole module is built around —
/// the shape taken from `goose`'s `ThinkingContentBlock`. Emma never reads it,
/// never composes one, and never re-derives one; it carries it out and back
/// unchanged, because a modified or missing signature is a 400 on the *next*
/// call rather than on the one that dropped it.
///
/// `Option` rather than goose's bare `String` because a streamed block that
/// carried no `signature_delta` has no signature, and sending `"signature": ""`
/// is not the same request as sending no key at all.
///
/// **What this type does not say, and it matters.** A signature is minted by
/// *a* model, and whether one model's signature is accepted when the same
/// conversation continues on another is **not established in this tree** —
/// nobody has tested it. Nothing here records which model produced the block,
/// so a mid-session model change cannot be reasoned about from a block alone.
/// The provenance that does exist is coarser: the session log's `goal` record
/// carries `model`, and `session::Continuity` compares it across a resume, so
/// today's granularity is per goal rather than per block.
///
/// That is a stated gap rather than a hidden one, and the shape above is what
/// keeps it fixable: the model-bound state is a field with a name, so
/// [`ContentBlock::is_model_bound`] can find every block a model change would
/// put at risk. Under the `raw_content` blob it replaced, that question could
/// not be asked at all without re-parsing the format by hand.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ThinkingBlock {
    pub thinking: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    /// Keys the provider put on this block that Emma does not model, echoed
    /// back untouched. See the module doc for what may live here and for the
    /// live call that made it necessary.
    #[serde(flatten, default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub extra: serde_json::Map<String, Value>,
}

/// Thinking the provider redacted. The whole block is opaque: `data` is the
/// encrypted payload and there is nothing else in it to model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct RedactedThinkingBlock {
    pub data: String,
    /// Keys the provider put on this block that Emma does not model, echoed
    /// back untouched. See the module doc for what may live here and for the
    /// live call that made it necessary.
    #[serde(flatten, default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub extra: serde_json::Map<String, Value>,
}

/// One tool call the model asked for.
///
/// `extra` is not decoration here: the API puts a `caller` key on every
/// `tool_use` block it emits, so a `ToolCall` that could not hold an unmodelled
/// key would never match a real one. See the module doc.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub input: Value,
    /// Keys the provider put on this block that Emma does not model, echoed
    /// back untouched. See the module doc for what may live here and for the
    /// live call that made it necessary.
    #[serde(flatten, default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub extra: serde_json::Map<String, Value>,
}

/// The answer to one [`ToolCall`], including the answer "it failed".
///
/// **`is_error` is load-bearing and is not an error type.** A failed tool is an
/// observation the loop continues from — `agent.rs` says so at the top of the
/// file and five failure classes in tustle-agent were measured ending turns
/// silently before it did. So this is one struct with a flag rather than a
/// `Result`: a `Result` would make "the tool failed" unrepresentable as a
/// message, which is precisely what has to be representable.
///
/// The flag is skipped when false so a successful result renders the two keys
/// it always rendered, and no third.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ToolResult {
    pub tool_use_id: String,
    pub content: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub is_error: bool,
    /// Keys the provider put on this block that Emma does not model, echoed
    /// back untouched. See the module doc for what may live here and for the
    /// live call that made it necessary.
    #[serde(flatten, default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub extra: serde_json::Map<String, Value>,
}

fn is_false(b: &bool) -> bool {
    !*b
}

impl ToolResult {
    pub fn ok(tool_use_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            tool_use_id: tool_use_id.into(),
            content: content.into(),
            is_error: false,
            ..Self::default()
        }
    }

    pub fn failed(tool_use_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            tool_use_id: tool_use_id.into(),
            content: content.into(),
            is_error: true,
            ..Self::default()
        }
    }
}

/// Put the unmodelled keys back on a rendered block.
///
/// Insertion rather than a raw copy so a provider key can never overwrite one
/// this client models: `extra` is only ever populated from keys the struct did
/// not claim, and this asserts that invariant instead of trusting it.
fn with_extra(mut v: Value, extra: &serde_json::Map<String, Value>) -> Value {
    if let Some(obj) = v.as_object_mut() {
        for (k, val) in extra {
            debug_assert!(
                !obj.contains_key(k),
                "`extra` carried {k}, which the typed block also renders"
            );
            obj.insert(k.clone(), val.clone());
        }
    }
    v
}

/// One block of content, in either direction.
#[derive(Debug, Clone, PartialEq)]
pub enum ContentBlock {
    Text(TextBlock),
    Thinking(ThinkingBlock),
    RedactedThinking(RedactedThinkingBlock),
    ToolUse(ToolCall),
    ToolResult(ToolResult),
    /// A block this client does not model, kept exactly as it arrived.
    ///
    /// **What may live here, exhaustively:** a whole content block, as the
    /// provider sent it, whose `type` is unknown to this client or whose fields
    /// do not fit the struct for that `type`. Nothing else. It is constructed
    /// in exactly one place — [`ContentBlock::from_value`], which is the
    /// provider's parse — and Emma never reads inside it, branches on it, or
    /// builds one.
    ///
    /// **Why this rather than nothing.** The alternative is an exhaustive enum
    /// that drops what it does not recognise, and this repository has already
    /// paid for that once: `anthropic::Partial::Opaque` exists because a
    /// `_ => {}` arm dropped thinking blocks, invisibly, until a later call
    /// failed on a signature nobody could find. Server-side tool blocks
    /// (`server_tool_use`, `web_search_tool_result`) are the same class of
    /// thing arriving next.
    ///
    /// **Why this rather than goose's `ProviderMetadata`.** That hangs a
    /// `Map<String, Value>` off *every* block for any key a provider wants
    /// back, which means every typed block has an untyped half and a reader
    /// cannot tell from the type which fields are real. This is narrower in
    /// both directions: it is per *block* rather than per *field*, and a block
    /// is either fully typed or fully opaque, never half of each. The cost is
    /// that one unmodelled field demotes a whole block; the benefit is that
    /// `ContentBlock::Text` means what it says.
    Passthrough(Value),
}

impl ContentBlock {
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text(TextBlock {
            text: text.into(),
            ..TextBlock::default()
        })
    }

    /// The wire object. Byte-for-byte what the Messages API documents, and what
    /// the previous `serde_json::Value` blocks rendered.
    pub fn to_value(&self) -> Value {
        match self {
            Self::Text(b) => with_extra(json!({ "type": "text", "text": b.text }), &b.extra),
            Self::Thinking(b) => {
                let mut v = json!({ "type": "thinking", "thinking": b.thinking });
                if let Some(sig) = &b.signature {
                    v["signature"] = json!(sig);
                }
                with_extra(v, &b.extra)
            }
            Self::RedactedThinking(b) => with_extra(
                json!({ "type": "redacted_thinking", "data": b.data }),
                &b.extra,
            ),
            Self::ToolUse(c) => with_extra(
                json!({ "type": "tool_use", "id": c.id, "name": c.name, "input": c.input }),
                &c.extra,
            ),
            Self::ToolResult(r) => {
                let mut v = json!({
                    "type": "tool_result",
                    "tool_use_id": r.tool_use_id,
                    "content": r.content,
                });
                if r.is_error {
                    v["is_error"] = json!(true);
                }
                with_extra(v, &r.extra)
            }
            Self::Passthrough(v) => v.clone(),
        }
    }

    /// Parse one wire block, keeping everything it carried.
    ///
    /// Two guarantees, and they fail in opposite directions if either is
    /// removed. The `extra` field on each struct catches keys this client does
    /// not model, so a `tool_use` that grew a `caller` is still a tool call —
    /// take it away and the loop stops seeing real tool calls, which is what a
    /// live run caught. The `.ok()` fallback catches a block this client cannot
    /// build at all, so an unknown `type` still travels — take that away and
    /// the turn is silently corrupted on replay.
    pub fn from_value(v: Value) -> Self {
        let Some(kind) = v.get("type").and_then(Value::as_str) else {
            return Self::Passthrough(v);
        };
        // The tag is removed rather than tolerated, because tolerating it would
        // mean loosening `deny_unknown_fields` — the one thing holding the
        // guarantee above up.
        let mut rest = match v.as_object() {
            Some(obj) => obj.clone(),
            None => return Self::Passthrough(v),
        };
        rest.remove("type");
        let rest = Value::Object(rest);
        let typed = match kind {
            "text" => serde_json::from_value(rest).map(Self::Text).ok(),
            "thinking" => serde_json::from_value(rest).map(Self::Thinking).ok(),
            "redacted_thinking" => serde_json::from_value(rest)
                .map(Self::RedactedThinking)
                .ok(),
            "tool_use" => serde_json::from_value(rest).map(Self::ToolUse).ok(),
            "tool_result" => serde_json::from_value(rest).map(Self::ToolResult).ok(),
            _ => None,
        };
        typed.unwrap_or(Self::Passthrough(v))
    }

    /// Whether this block carries state only the model that produced it can
    /// validate.
    ///
    /// **This exists to keep a question answerable, not because anything asks
    /// it yet.** A thinking signature is minted by one model; whether it is
    /// accepted when the same conversation continues on a *different* model is
    /// untested here, and an in-session `/model` command is the first thing
    /// that would need to know. Under the `raw_content` blob this replaced, the
    /// only way to find those blocks was to re-parse Anthropic's format at the
    /// call site — so a fallback like "strip the model-bound blocks and retry"
    /// could not be written without undoing the abstraction.
    ///
    /// `Passthrough` counts, and that is the conservative direction on purpose:
    /// this client does not know what is in a block it could not model, so it
    /// must not claim the block is portable.
    ///
    /// A plain text block is not model-bound, which is why the existing
    /// compaction path is already a working escape hatch: a summarised chapter
    /// is [`Content::Text`] and carries no signatures at all.
    pub fn is_model_bound(&self) -> bool {
        match self {
            Self::Thinking(t) => t.signature.is_some(),
            Self::RedactedThinking(_) | Self::Passthrough(_) => true,
            Self::Text(_) | Self::ToolUse(_) | Self::ToolResult(_) => false,
        }
    }

    /// The `tool_use` id this block is, or answers — the pairing the API checks
    /// both ways round.
    pub fn tool_use_id(&self) -> Option<&str> {
        match self {
            Self::ToolUse(c) => Some(&c.id),
            _ => None,
        }
    }
}

impl Serialize for ContentBlock {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.to_value().serialize(s)
    }
}

impl<'de> Deserialize<'de> for ContentBlock {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(Self::from_value(Value::deserialize(d)?))
    }
}

// endregion: The blocks

// region: A message's content
// ---------------------------------------------------------------------------
// A message's content
//
// The API takes either a bare string or an array of blocks, and both forms are
// on the wire today — so this is two variants rather than one, and it renders
// as whichever it holds.
// ---------------------------------------------------------------------------

/// Everything one message carries.
///
/// Two variants because the API takes two forms and Emma sends both: a user's
/// typed words go as a plain string, and anything with tool traffic in it goes
/// as blocks. Collapsing them to `Blocks` alone would be tidier and would
/// change the bytes of every ordinary user turn, which is a cache prefix nobody
/// asked to invalidate.
#[derive(Debug, Clone, PartialEq)]
pub enum Content {
    Text(String),
    Blocks(Vec<ContentBlock>),
}

impl Content {
    pub fn blocks(&self) -> &[ContentBlock] {
        match self {
            Self::Text(_) => &[],
            Self::Blocks(b) => b,
        }
    }

    /// How many bytes this is on the wire.
    ///
    /// The estimator that decides whether a prefix clears the cache minimum
    /// counts these, so it has to be the rendered length rather than the length
    /// of the text inside — the quotes and the escapes are bytes the provider
    /// tokenises too.
    pub fn wire_len(&self) -> usize {
        serde_json::to_string(self).map(|s| s.len()).unwrap_or(0)
    }
}

/// The rendered JSON, which is what `Value`'s own `Display` gave before this
/// was a type. Kept because half a dozen assertions ask "did the model see this
/// string anywhere in the message", and the honest answer to that is about the
/// bytes that were sent rather than about any one variant.
impl std::fmt::Display for Content {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match serde_json::to_string(self) {
            Ok(s) => f.write_str(&s),
            Err(e) => write!(f, "<unrenderable content: {e}>"),
        }
    }
}

impl Serialize for Content {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Text(t) => t.serialize(s),
            Self::Blocks(b) => b.serialize(s),
        }
    }
}

impl<'de> Deserialize<'de> for Content {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        match Value::deserialize(d)? {
            Value::String(t) => Ok(Self::Text(t)),
            Value::Array(items) => Ok(Self::Blocks(
                items.into_iter().map(ContentBlock::from_value).collect(),
            )),
            other => Err(D::Error::custom(format!(
                "message content must be a string or an array of blocks, got {other}"
            ))),
        }
    }
}

// endregion: A message's content

// region: Tests
// ---------------------------------------------------------------------------
// Tests
//
// One claim, in several shapes: what goes in comes out. The interesting cases
// are the ones where the typed model is *wrong* about a block — an unknown
// type, and a known type with an extra field — because those are the ones a
// silently-lossy design passes and this one has to fail.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trips(v: Value) -> ContentBlock {
        let block = ContentBlock::from_value(v.clone());
        assert_eq!(
            block.to_value(),
            v,
            "the block did not round-trip: {block:?}"
        );
        block
    }

    #[test]
    fn every_modelled_block_round_trips_to_the_same_bytes() {
        assert!(matches!(
            round_trips(json!({ "type": "text", "text": "hello" })),
            ContentBlock::Text(_)
        ));
        assert!(matches!(
            round_trips(json!({ "type": "thinking", "thinking": "hm", "signature": "sig-abc" })),
            ContentBlock::Thinking(_)
        ));
        // A streamed block that never carried a signature must not grow one.
        assert!(matches!(
            round_trips(json!({ "type": "thinking", "thinking": "hm" })),
            ContentBlock::Thinking(ThinkingBlock {
                signature: None,
                ..
            })
        ));
        assert!(matches!(
            round_trips(json!({ "type": "redacted_thinking", "data": "AAAA" })),
            ContentBlock::RedactedThinking(_)
        ));
        assert!(matches!(
            round_trips(json!({ "type": "tool_use", "id": "t1", "name": "Read", "input": {} })),
            ContentBlock::ToolUse(_)
        ));
        assert!(matches!(
            round_trips(json!({ "type": "tool_result", "tool_use_id": "t1", "content": "ok" })),
            ContentBlock::ToolResult(_)
        ));
        assert!(matches!(
            round_trips(
                json!({ "type": "tool_result", "tool_use_id": "t1", "content": "no", "is_error": true })
            ),
            ContentBlock::ToolResult(_)
        ));
    }

    #[test]
    fn a_signature_survives_the_type_it_is_carried_in() {
        // The failure this whole module has to not have: a thinking block whose
        // signature was re-derived or dropped is rejected on the *next* call,
        // with an error naming a token nothing in the transcript mentions.
        let block = ContentBlock::from_value(
            json!({ "type": "thinking", "thinking": "step one", "signature": "sig-xyz" }),
        );
        let ContentBlock::Thinking(t) = &block else {
            panic!("not typed as thinking: {block:?}");
        };
        assert_eq!(t.signature.as_deref(), Some("sig-xyz"));
        assert_eq!(block.to_value()["signature"], "sig-xyz");
    }

    #[test]
    fn an_unknown_block_type_is_carried_rather_than_dropped() {
        // The bug `Partial::Opaque` was written for, at the type level. A
        // server-side tool block is the next real instance of it.
        let v = json!({
            "type": "server_tool_use",
            "id": "srvtoolu_1",
            "name": "web_search",
            "input": { "query": "rust" }
        });
        let block = round_trips(v);
        assert!(matches!(block, ContentBlock::Passthrough(_)), "{block:?}");
    }

    /// A `tool_use` block exactly as `claude-opus-5` sends one, `caller` and all.
    ///
    /// **This is a regression test with a receipt.** The first version of this
    /// module decoded strictly and demoted any block carrying an unmodelled
    /// key. Every wire test passed, the whole suite was green, and the first
    /// live goal ended on turn one reported as "answered — no tools were
    /// needed": `caller` is on every real `tool_use`, so no real tool call ever
    /// decoded as one. The bytes were perfect and the agent did not work.
    ///
    /// So the two claims here are not the same claim, and both are load-bearing:
    /// the block is still a **tool call** (the loop can act on it) and it still
    /// round-trips **byte for byte** (the provider gets back what it sent).
    #[test]
    fn a_real_tool_use_block_is_a_tool_call_and_still_round_trips() {
        let v = json!({
            "caller": { "type": "direct" },
            "id": "toolu_01FJxkRrmirASsf79TJLvggU",
            "input": { "file_path": "absent.txt" },
            "name": "Read",
            "type": "tool_use"
        });
        let block = round_trips(v);
        let ContentBlock::ToolUse(call) = &block else {
            panic!("a real tool_use block did not decode as a tool call: {block:?}");
        };
        assert_eq!(call.name, "Read");
        assert_eq!(call.id, "toolu_01FJxkRrmirASsf79TJLvggU");
        assert_eq!(call.input, json!({ "file_path": "absent.txt" }));
        assert_eq!(call.extra["caller"], json!({ "type": "direct" }));
    }

    #[test]
    fn an_unmodelled_key_on_any_known_block_survives() {
        // The same rule on the other block types, so the fix is a property of
        // the model rather than a patch to the one type that was caught.
        for v in [
            json!({ "citations": [{ "url": "https://x" }], "text": "see the docs", "type": "text" }),
            json!({ "thinking": "hm", "tier": "extended", "type": "thinking" }),
            json!({ "content": "ok", "cache_hint": true, "tool_use_id": "t1", "type": "tool_result" }),
        ] {
            let block = round_trips(v);
            assert!(
                !matches!(block, ContentBlock::Passthrough(_)),
                "a known block type was demoted rather than carrying its extra key: {block:?}"
            );
        }
    }

    #[test]
    fn model_bound_state_is_findable_rather_than_merely_preserved() {
        // Preserving the state is not the same as being able to locate it. An
        // in-session model change needs the second, and the blob this replaced
        // could only do the first — so this pins the distinction rather than
        // the mechanism, which is not built.
        let bound = [
            ContentBlock::from_value(
                json!({ "type": "thinking", "thinking": "x", "signature": "s" }),
            ),
            ContentBlock::from_value(json!({ "type": "redacted_thinking", "data": "AAAA" })),
            ContentBlock::from_value(json!({ "type": "server_tool_use", "id": "s1" })),
        ];
        for b in &bound {
            assert!(b.is_model_bound(), "{b:?}");
        }

        let portable = [
            ContentBlock::text("hello"),
            ContentBlock::from_value(json!({ "type": "thinking", "thinking": "unsigned" })),
            ContentBlock::from_value(
                json!({ "type": "tool_use", "id": "t1", "name": "Read", "input": {} }),
            ),
            ContentBlock::ToolResult(ToolResult::ok("t1", "fine")),
        ];
        for b in &portable {
            assert!(!b.is_model_bound(), "{b:?}");
        }

        // The escape hatch a fallback would use, stated as an assertion: a
        // summarised turn is plain text, so it carries nothing model-bound.
        assert!(Content::Text("a summary".into())
            .blocks()
            .iter()
            .all(|b| !b.is_model_bound()));
    }

    #[test]
    fn content_renders_as_the_two_forms_the_api_takes() {
        let text = Content::Text("hi".into());
        assert_eq!(serde_json::to_value(&text).unwrap(), json!("hi"));
        let blocks = Content::Blocks(vec![ContentBlock::text("hi")]);
        assert_eq!(
            serde_json::to_value(&blocks).unwrap(),
            json!([{ "type": "text", "text": "hi" }])
        );
        // …and back, so a session file written from either form folds to it.
        for v in [json!("hi"), json!([{ "type": "text", "text": "hi" }])] {
            let back: Content = serde_json::from_value(v.clone()).unwrap();
            assert_eq!(serde_json::to_value(&back).unwrap(), v);
        }
    }

    #[test]
    fn wire_len_counts_the_rendered_bytes_not_the_text() {
        // The cache size gate is fed by this, and it used to be
        // `Value::to_string().len()` on the same content — so it has to keep
        // counting the quotes.
        assert_eq!(Content::Text("hi".into()).wire_len(), 4);
        assert_eq!(
            Content::Text("hi".into()).wire_len(),
            json!("hi").to_string().len()
        );
    }
}

// endregion: Tests
