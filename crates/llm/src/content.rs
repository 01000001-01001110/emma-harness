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
    /// Pictures this result carries alongside its prose.
    ///
    /// Empty for every tool that returns text, which is all of them but one,
    /// and a result with no images renders exactly the two keys it always
    /// rendered. See [`ToolImage`] for the two states one of these can be in
    /// and why the difference is what keeps a session log replayable.
    ///
    /// Skipped by serde because the rendering is hand-written in
    /// [`ContentBlock::to_value`]: an image cannot be a sibling key of
    /// `content` on the wire, it has to be an element *inside* it.
    #[serde(skip)]
    pub images: Vec<ToolImage>,
    /// Keys the provider put on this block that Emma does not model, echoed
    /// back untouched. See the module doc for what may live here and for the
    /// live call that made it necessary.
    #[serde(flatten, default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub extra: serde_json::Map<String, Value>,
}

/// One picture attached to a tool result, in one of exactly two states.
///
/// **Carrying the bytes, `data: Some`.** This is what goes to a provider. It is
/// what the capturing tool builds and what the loop hands to the wire.
///
/// **Naming the file, `data: None`.** This is what goes into the session log.
/// A bounded screenshot is still hundreds of kilobytes of base64, and a log
/// line per capture at that size makes the transcript unreadable by anything,
/// including the fold that has to walk it. So the log records the media type,
/// the byte count and the path the bytes are sitting at, and
/// [`ToolImage::for_log`] is the one place the demotion happens.
///
/// The cost is stated rather than hidden: replay depends on a file outside the
/// log. `session::fold` reads it back when it is there and, when it is not,
/// says in the result text that an image was elided instead of pretending the
/// turn was always prose. Neither half is silent.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ToolImage {
    /// `image/png`, `image/jpeg`. Sent verbatim as the source `media_type`.
    pub media_type: String,
    /// The bytes, base64. `None` is the log form: see the type doc.
    pub data: Option<String>,
    /// Where the full-resolution capture is on disk, when the tool kept one.
    pub path: Option<String>,
    /// How large the payload is, counted as base64 characters rather than as
    /// decoded bytes, because that is the number both directions can agree on:
    /// a log line that has dropped `data` still says how big it was, and a
    /// block read back off the wire can recompute it without decoding.
    pub bytes: u64,
}

impl ToolImage {
    /// The wire form: a media type and the payload that travels.
    pub fn base64(media_type: impl Into<String>, data: impl Into<String>) -> Self {
        let data = data.into();
        Self {
            media_type: media_type.into(),
            bytes: data.len() as u64,
            data: Some(data),
            path: None,
        }
    }

    /// Where the full-resolution original is, for the log form to name.
    pub fn at_path(mut self, path: impl Into<String>) -> Self {
        self.path = Some(path.into());
        self
    }

    /// The same image with the bytes taken out. See the type doc.
    pub fn for_log(&self) -> Self {
        Self {
            data: None,
            ..self.clone()
        }
    }

    /// The base64 payload, when this image is carrying one. `None` is the log
    /// form, which has nothing to send.
    pub fn wire_data(&self) -> Option<&str> {
        self.data.as_deref()
    }

    /// The wire block.
    ///
    /// The log form renders too, and deliberately as a source type no provider
    /// has heard of: `emma_file` is what `session::fold` reads back, and it is
    /// a 400 rather than a silently missing picture if one ever reaches a
    /// provider. Nothing sends a log-form result — [`ToolResult::for_log`] is
    /// the only thing that makes one — and this is the second gate on that.
    fn to_value(&self) -> Value {
        match &self.data {
            Some(data) => json!({
                "type": "image",
                "source": {
                    "type": "base64",
                    "media_type": self.media_type,
                    "data": data,
                }
            }),
            None => json!({
                "type": "image",
                "source": {
                    "type": "emma_file",
                    "media_type": self.media_type,
                    "path": self.path,
                    "bytes": self.bytes,
                }
            }),
        }
    }

    /// Read one back. `None` when the block is not an image this models, which
    /// keeps the caller's fallback to `Passthrough` honest.
    ///
    /// **`media_type` is required, and the fork this was ported from defaulted
    /// it to `""`.** That difference is the module's own rule applied: a source
    /// object with no media type is a shape this client cannot render back
    /// byte-for-byte, so reading it would turn a round trip into a rewrite —
    /// the block would come out carrying `"media_type": ""` that the provider
    /// never sent. `a_tool_result_whose_content_is_a_block_array_is_kept_whole`
    /// is the test that caught it, and it caught it because it asserts the
    /// bytes rather than the fields.
    fn from_value(v: &Value) -> Option<Self> {
        if v.get("type").and_then(Value::as_str)? != "image" {
            return None;
        }
        let source = v.get("source")?;
        let media_type = source
            .get("media_type")
            .and_then(Value::as_str)?
            .to_string();
        match source.get("type").and_then(Value::as_str)? {
            "base64" => {
                let data = source.get("data").and_then(Value::as_str)?.to_string();
                let bytes = data.len() as u64;
                Some(Self {
                    media_type,
                    data: Some(data),
                    path: None,
                    bytes,
                })
            }
            "emma_file" => Some(Self {
                media_type,
                data: None,
                path: source
                    .get("path")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                bytes: source.get("bytes").and_then(Value::as_u64).unwrap_or(0),
            }),
            _ => None,
        }
    }
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

    /// The same result with every image demoted to its path. What a session
    /// log stores; never what a provider is sent. See [`ToolImage`].
    pub fn for_log(&self) -> Self {
        Self {
            images: self.images.iter().map(ToolImage::for_log).collect(),
            ..self.clone()
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

/// Rebuild the array form of a `tool_result` block.
///
/// Exactly the inverse of what [`ContentBlock::to_value`] writes, and no more
/// tolerant than that: at most one leading text block, then images. A result
/// whose content array holds anything else came from somewhere this client did
/// not write it, so it returns `None` and travels whole as `Passthrough`
/// instead of being half-read.
fn tool_result_from_array(rest: &Value) -> Option<ToolResult> {
    let mut obj = rest.as_object()?.clone();
    let items = obj.remove("content")?;
    let items = items.as_array()?;
    let mut content = String::new();
    let mut images = Vec::new();
    for (i, item) in items.iter().enumerate() {
        match item.get("type").and_then(Value::as_str)? {
            "text" if i == 0 && images.is_empty() => {
                content = item.get("text").and_then(Value::as_str)?.to_string();
            }
            "image" => images.push(ToolImage::from_value(item)?),
            _ => return None,
        }
    }
    let is_error = obj
        .remove("is_error")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let tool_use_id = obj.remove("tool_use_id")?.as_str()?.to_string();
    Some(ToolResult {
        tool_use_id,
        content,
        is_error,
        images,
        extra: obj,
    })
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
                // Two renderings, and which one is used is decided by the
                // result rather than by a flag: a result with no images renders
                // the string form it has always rendered, so no existing tool
                // moves a byte. Only a result carrying a picture becomes the
                // array form, which is the only shape the API accepts an image
                // in.
                let content = if r.images.is_empty() {
                    json!(r.content)
                } else {
                    let mut blocks = Vec::new();
                    if !r.content.is_empty() {
                        blocks.push(json!({ "type": "text", "text": r.content }));
                    }
                    blocks.extend(r.images.iter().map(ToolImage::to_value));
                    Value::Array(blocks)
                };
                let mut v = json!({
                    "type": "tool_result",
                    "tool_use_id": r.tool_use_id,
                    "content": content,
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
            // Hand-parsed rather than derived, because `content` is a string in
            // one form and an array of blocks in the other, and serde cannot be
            // told that without giving up `deny_unknown_fields` on the rest of
            // the struct. `tool_result_from_array` returns `None` for anything
            // it cannot rebuild exactly, which drops the block to `Passthrough`
            // under the same rule as every other unmodelled shape.
            "tool_result" if rest.get("content").is_some_and(Value::is_array) => {
                tool_result_from_array(&rest).map(Self::ToolResult)
            }
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

    /// `ToolResult::content` is a `String`, and a picture is not a string, so
    /// a result carrying one renders `content` as an array instead. Both forms
    /// have to survive a round trip or a resumed conversation loses either its
    /// prose or its picture.
    #[test]
    fn a_tool_result_carrying_an_image_round_trips_in_both_of_its_two_forms() {
        let wire = json!({
            "type": "tool_result",
            "tool_use_id": "t1",
            "content": [
                { "type": "text", "text": "Captured display 1." },
                { "type": "image", "source": {
                    "type": "base64", "media_type": "image/jpeg", "data": "aGk=" }}
            ]
        });
        let ContentBlock::ToolResult(r) = ContentBlock::from_value(wire.clone()) else {
            panic!("the array form was demoted to a passthrough");
        };
        assert_eq!(r.content, "Captured display 1.");
        assert_eq!(r.images.len(), 1);
        assert_eq!(r.images[0].media_type, "image/jpeg");
        assert_eq!(r.images[0].data.as_deref(), Some("aGk="));
        assert_eq!(r.images[0].bytes, 4);
        assert_eq!(ContentBlock::ToolResult(r).to_value(), wire);

        // The log form, which is what a session file holds. `emma_file` is a
        // source type no provider has heard of, on purpose.
        let logged = json!({
            "type": "tool_result",
            "tool_use_id": "t1",
            "content": [
                { "type": "text", "text": "Captured display 1." },
                { "type": "image", "source": {
                    "type": "emma_file",
                    "media_type": "image/jpeg",
                    "path": "C:/src/emma/shot.png",
                    "bytes": 4 }}
            ]
        });
        let ContentBlock::ToolResult(r) = ContentBlock::from_value(logged.clone()) else {
            panic!("the log form was demoted to a passthrough");
        };
        assert_eq!(r.images[0].data, None);
        assert_eq!(r.images[0].path.as_deref(), Some("C:/src/emma/shot.png"));
        assert_eq!(r.images[0].bytes, 4);
        assert_eq!(ContentBlock::ToolResult(r).to_value(), logged);
    }

    /// The demotion the session log depends on: the bytes go, the size stays.
    /// Without the size a log line cannot say how big the picture it dropped
    /// was, and a reader of the transcript cannot tell an elided screenshot
    /// from a tool that returned nothing.
    #[test]
    fn the_log_form_drops_the_bytes_and_keeps_everything_else() {
        let mut r = ToolResult::ok("t1", "Captured display 1.");
        r.images = vec![ToolImage::base64("image/jpeg", "aGk=").at_path("C:/src/emma/shot.png")];
        let logged = r.for_log();

        assert_eq!(logged.images[0].data, None, "the bytes must not be logged");
        assert_eq!(logged.images[0].bytes, 4, "the size survives the elision");
        assert_eq!(
            logged.images[0].path.as_deref(),
            Some("C:/src/emma/shot.png")
        );
        assert_eq!(logged.images[0].media_type, "image/jpeg");
        assert_eq!(logged.content, r.content);
        // The original is untouched: the loop still has the bytes to send after
        // it has written the log line.
        assert_eq!(r.images[0].data.as_deref(), Some("aGk="));
    }

    /// The half of the two-form rendering that no test would otherwise notice
    /// breaking: every tool but one returns prose, and a result with no images
    /// must render the exact two keys it rendered before images existed.
    #[test]
    fn a_result_with_no_images_still_renders_the_string_form() {
        let ok = ContentBlock::ToolResult(ToolResult::ok("t1", "fine")).to_value();
        assert_eq!(
            ok,
            json!({ "type": "tool_result", "tool_use_id": "t1", "content": "fine" })
        );
        let failed = ContentBlock::ToolResult(ToolResult::failed("t1", "no")).to_value();
        assert_eq!(
            failed,
            json!({
                "type": "tool_result", "tool_use_id": "t1",
                "content": "no", "is_error": true
            })
        );
        // Byte-identical, not merely equal as JSON: `content` must still be a
        // string and not a one-element array holding the same text.
        assert!(ok["content"].is_string(), "{ok}");
        assert!(failed["content"].is_string(), "{failed}");
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

    /// **If this breaks:** a tool result the model sent back — an image, a
    /// search result, anything the API renders as blocks — is decoded with its
    /// payload replaced by an empty string, and nothing says so.
    ///
    /// `ToolResult::content` is a `String` because that is what Emma *writes*.
    /// It is not what the API accepts: `tool_result.content` may be an array of
    /// blocks, and a session file or a replayed transcript can carry one. The
    /// two ways that can go wrong pull in opposite directions and only one of
    /// them is loud, so both are asserted here — the block must not decode as a
    /// `ToolResult` whose content silently became `""`, and it must not be
    /// dropped either. Demotion to `Passthrough` is the answer that loses
    /// nothing.
    #[test]
    fn a_tool_result_whose_content_is_a_block_array_is_kept_whole() {
        let v = json!({
            "type": "tool_result",
            "tool_use_id": "toolu_1",
            "content": [
                { "type": "text", "text": "line one" },
                { "type": "image", "source": { "type": "base64", "data": "AAAA" } }
            ]
        });
        let block = round_trips(v.clone());
        assert!(
            matches!(block, ContentBlock::Passthrough(_)),
            "a block shape this struct cannot hold must stay opaque rather than \
             being decoded lossily: {block:?}"
        );
        // The anti-vacuity half: `round_trips` would also pass if the payload
        // came back as an empty array, so name what has to still be inside.
        assert_eq!(block.to_value()["content"][0]["text"], "line one");
        assert_eq!(
            block.to_value()["content"][1]["source"]["data"],
            "AAAA",
            "the second block was dropped"
        );
    }

    /// **If this breaks:** a block with no usable `type` is decoded as whatever
    /// the fallback happens to name, and its fields are read as that type's —
    /// which is how a turn gets quietly rewritten on replay.
    ///
    /// `from_value` reads the tag with `and_then(Value::as_str)`, so a missing
    /// tag and a null tag both land in the same place. Neither is a shape the
    /// live API produces; both are shapes a truncated session file, a
    /// hand-edited transcript or a middlebox produces, and the decoder has no
    /// way to tell those apart from a response. There is nothing to interpret,
    /// so the only correct answer is to interpret nothing and keep the bytes.
    #[test]
    fn a_block_with_no_readable_type_is_kept_rather_than_guessed_at() {
        for v in [
            json!({ "text": "looks like a text block, is not tagged as one" }),
            json!({ "type": null, "text": "tagged with a null" }),
            json!({ "type": 7, "text": "tagged with a number" }),
        ] {
            let block = round_trips(v.clone());
            assert!(
                matches!(block, ContentBlock::Passthrough(_)),
                "an untagged block was guessed at rather than carried: {block:?}"
            );
            // It must also not answer questions about itself that it cannot
            // know — an unreadable block is conservatively model-bound.
            assert!(block.is_model_bound(), "{block:?}");
            assert_eq!(block.tool_use_id(), None);
        }
    }

    /// **If this breaks:** a corrupt session file loads as a conversation with
    /// a message whose content silently became empty, and the next call is made
    /// against a history that is not the one on disk.
    ///
    /// `Content` accepts the two forms the API takes and refuses everything
    /// else with a message naming what it got. The refusal is the whole reason
    /// this is a hand-written `Deserialize` rather than an untagged enum: an
    /// untagged enum's failure mode is to try each variant and report nothing
    /// useful, and a lenient one's is to fall through to `Text(String::new())`.
    /// A number, a bare object and a null are all one lenient arm away from
    /// being an empty message.
    #[test]
    fn a_message_content_that_is_neither_a_string_nor_blocks_is_refused_loudly() {
        for v in [
            json!(42),
            json!(null),
            json!({ "type": "text" }),
            json!(true),
        ] {
            let err = serde_json::from_value::<Content>(v.clone())
                .expect_err(&format!("content {v} was accepted"));
            let shown = err.to_string();
            assert!(
                shown.contains("string or an array of blocks"),
                "the refusal must say what shape was expected: {shown}"
            );
        }

        // The control, so the refusal above cannot be a decoder that refuses
        // everything: both real forms still load, including an empty array.
        for v in [
            json!("hi"),
            json!([]),
            json!([{ "type": "text", "text": "hi" }]),
        ] {
            let back: Content = serde_json::from_value(v.clone()).expect("a real form was refused");
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
