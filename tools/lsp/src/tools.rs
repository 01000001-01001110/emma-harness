//! The four tools, and the one path every position-based question takes.
//!
//! Each tool is a thin shell — validate, contain, locate, ask, render — because
//! everything hard is in the other modules and repeating any of it here is how
//! four tools come to disagree about containment.
//!
//! **The shared shape.** `FindReferences`, `GoToDefinition` and `Hover` take the
//! same three arguments: a file, a 1-based line, and the symbol's name. See
//! [`crate::doc::locate`] for why a name rather than a column — asking a model
//! for a column offset gets a plausible number, and a plausible number is a
//! wrong answer about the symbol next door. The shape also means `Grep` output
//! feeds straight in, which is the actual workflow: grep to find candidate
//! lines, then ask the compiler which of them is real.
//!
//! **What every one of them does before asking.** Resolve the path through
//! `tools/fs`'s containment, refuse anything that is not Rust, read the file
//! from disk, and push the current bytes to the server. That last step is not
//! optional: an agent's rhythm is `Edit` then ask, and a server answering from
//! the version it opened before the edit returns positions into a file that no
//! longer exists.

use std::path::PathBuf;
use std::sync::Arc;

use emma_tool_api::{Tool, ToolCtx, ToolError, ToolMeta, ToolOutcome};
use emma_tools_fs::path;
use serde_json::{json, Value};

use crate::args;
use crate::client::{Answer, Client};
use crate::doc::{self, Position};
use crate::pool::Pool;
use crate::render;
use crate::server::SUPPORTED_EXTENSION;

// region: The shared preamble
// ---------------------------------------------------------------------------
// The shared preamble
//
// Containment, the language gate, the read, the position, and the sync — in one
// place so that four tools cannot each get a different part of it wrong.
// ---------------------------------------------------------------------------

/// Everything a tool needs after the preamble has run.
struct Prepared {
    client: Arc<Client>,
    root: PathBuf,
    file: PathBuf,
    text: String,
}

/// Contain the path, refuse a non-Rust file, read it, and get a server that
/// knows about it.
///
/// Note the order. Containment first, before the file is read and before a
/// server is started — a path outside the root must not so much as cause a
/// process to spawn.
async fn prepare(
    pool: &Pool,
    ctx: &ToolCtx,
    tool: &str,
    args_v: &Value,
) -> Result<Prepared, ToolError> {
    let root = path::root(ctx)?;
    let raw = args::req_str(args_v, tool, "file_path")?;
    let (file, meta) = path::resolve_existing(&root, raw)?;
    if meta.is_dir() {
        return Err(ToolError::BadArguments(format!("{raw} is a directory")));
    }

    // The language gate, and the promise never to answer with something else.
    let extension = file
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    if extension != SUPPORTED_EXTENSION {
        return Err(crate::server::unsupported_language(&path::display(
            &root, &file,
        )));
    }

    let text = std::fs::read_to_string(&file)
        .map_err(|e| ToolError::Failed(format!("{raw} could not be read: {e}")))?;

    let client = pool.client(&root).await?;
    client.sync_document(&file, &text);
    Ok(Prepared {
        client,
        root,
        file,
        text,
    })
}

/// The preamble plus the position, for the three tools that need one.
async fn prepare_at(
    pool: &Pool,
    ctx: &ToolCtx,
    tool: &str,
    args_v: &Value,
) -> Result<(Prepared, Position), ToolError> {
    // The line and the symbol are read *before* the file is opened or a server
    // started, so a malformed call costs nothing. `locate` needs the text, so
    // the position itself cannot be computed until after — but its two cheap
    // refusals can be, and are, in `validate_args`.
    let line = args::req_u64(args_v, tool, "line")?;
    let symbol = args::req_str(args_v, tool, "symbol")?.to_string();
    let occurrence = args::opt_u64(args_v, tool, "occurrence")?;

    let prepared = prepare(pool, ctx, tool, args_v).await?;
    let (position, _) = doc::locate(&prepared.text, line, &symbol, occurrence)?;
    Ok((prepared, position))
}

fn text_document_position(file: &std::path::Path, position: Position) -> Value {
    json!({
        "textDocument": { "uri": doc::to_uri(file) },
        "position": { "line": position.line, "character": position.character },
    })
}

/// The arguments the three position tools share, and their schema.
const POSITION_KEYS: &[&str] = &["file_path", "line", "symbol", "occurrence"];

fn position_schema(extra: Option<(&str, Value)>) -> Value {
    let mut properties = json!({
        "file_path": { "type": "string", "description": "A Rust file inside the working directory." },
        "line": { "type": "integer", "minimum": 1, "description": "1-based line number the symbol appears on." },
        "symbol": { "type": "string", "description": "The symbol's name as it is written on that line. Emma finds the column; it refuses rather than guessing if the name appears more than once." },
        "occurrence": { "type": "integer", "minimum": 1, "description": "Which occurrence on the line, 1-based. Only needed when the name appears more than once." }
    });
    if let Some((name, spec)) = extra {
        properties[name] = spec;
    }
    json!({
        "type": "object",
        "properties": properties,
        "required": ["file_path", "line", "symbol"],
        "additionalProperties": false,
    })
}

/// The cheap half of validation, shared. The expensive half — is the symbol
/// actually on that line — needs the file and lives in `invoke`, as
/// `BadArguments`, exactly as the trait's documentation describes.
fn validate_position_args(tool: &str, args_v: &Value, keys: &[&str]) -> Result<(), ToolError> {
    args::deny_unknown(args_v, tool, keys)?;
    args::req_str(args_v, tool, "file_path")?;
    let line = args::req_u64(args_v, tool, "line")?;
    if line == 0 {
        return Err(ToolError::BadArguments(format!(
            "{tool}.line is 1-based; there is no line 0"
        )));
    }
    if args::req_str(args_v, tool, "symbol")?.is_empty() {
        return Err(ToolError::BadArguments(format!("{tool}.symbol is empty")));
    }
    if let Some(0) = args::opt_u64(args_v, tool, "occurrence")? {
        return Err(ToolError::BadArguments(format!(
            "{tool}.occurrence is 1-based; there is no occurrence 0"
        )));
    }
    Ok(())
}

/// Every tool here declares the same three bits, and the reasoning is common
/// enough to be worth stating once.
///
/// **`read_only: true`, and what makes it true rather than convenient.** These
/// tools spawn a process, which the field's own documentation lists as a thing
/// `read_only` forbids — and yet `WebFetch` declares `true` while driving a
/// whole browser, because the question the bit actually asks is "can this damage
/// this machine". The answer here is no, and it is no *by construction* rather
/// than by hope: `client::INIT_OPTIONS` turns off `cargo check`, build scripts
/// and proc macros, which are the three ways rust-analyzer writes to a project
/// or runs its code. `tests/read_only.rs` asserts the tree is byte-identical
/// after every read tool has been run against it, the same way `tools/fs` does.
///
/// **`reaches_network: false`**, made true by `--offline` on every cargo
/// invocation. Without it `cargo metadata` fetches from crates.io for a project
/// whose dependencies are not vendored — real egress, from a tool the gate lets
/// through silently.
///
/// **`idempotent: true`** follows from `read_only: true`; `Registry::register`
/// refuses the other combination, and rightly.
const READ_ONLY: ToolMeta = ToolMeta {
    read_only: true,
    reaches_network: false,
    idempotent: true,
};

// endregion: The shared preamble

// region: FindReferences
// ---------------------------------------------------------------------------
// FindReferences
//
// The one that most beats grep, and the one whose empty result is most
// dangerous — which is the same fact twice.
// ---------------------------------------------------------------------------

pub struct FindReferences {
    pool: Arc<Pool>,
}

impl FindReferences {
    pub fn new(pool: Arc<Pool>) -> Self {
        Self { pool }
    }
}

const REFERENCES_KEYS: &[&str] = &[
    "file_path",
    "line",
    "symbol",
    "occurrence",
    "include_declaration",
];

#[async_trait::async_trait]
impl Tool for FindReferences {
    fn name(&self) -> &'static str {
        "FindReferences"
    }

    fn description(&self) -> &str {
        include_str!("descriptions/find_references.md")
    }

    fn input_schema(&self) -> Value {
        position_schema(Some((
            "include_declaration",
            json!({
                "type": "boolean",
                "description": "Include the definition itself among the results. Defaults to true.",
            }),
        )))
    }

    fn meta(&self) -> ToolMeta {
        READ_ONLY
    }

    fn validate_args(&self, args_v: &Value) -> Result<(), ToolError> {
        validate_position_args("FindReferences", args_v, REFERENCES_KEYS)
    }

    async fn invoke(
        &self,
        ctx: &ToolCtx,
        args_v: Value,
    ) -> anyhow::Result<Result<ToolOutcome, ToolError>> {
        Ok(self.run(ctx, args_v).await)
    }
}

impl FindReferences {
    async fn run(&self, ctx: &ToolCtx, args_v: Value) -> Result<ToolOutcome, ToolError> {
        self.validate_args(&args_v)?;
        let include_declaration = match args_v.get("include_declaration") {
            None | Some(Value::Null) => true,
            Some(Value::Bool(b)) => *b,
            Some(other) => {
                return Err(ToolError::BadArguments(format!(
                    "FindReferences.include_declaration must be true or false, got {}",
                    args::type_name(other)
                )))
            }
        };
        let (prepared, position) = prepare_at(&self.pool, ctx, "FindReferences", &args_v).await?;

        let mut params = text_document_position(&prepared.file, position);
        params["context"] = json!({ "includeDeclaration": include_declaration });
        let Answer {
            value,
            readiness,
            health,
        } = prepared
            .client
            .request("textDocument/references", params)
            .await?;

        Ok(render::locations(
            &prepared.root,
            prepared.client.server(),
            readiness,
            health.as_deref(),
            "references",
            render::parse_locations(&value),
        ))
    }
}

// endregion: FindReferences

// region: GoToDefinition
// ---------------------------------------------------------------------------
// GoToDefinition
//
// The same machinery pointed the other way. Kept as its own tool rather than a
// mode of the one above because "where is this defined" and "what uses this"
// are different questions with different answers, and a `mode` parameter is a
// thing models get wrong silently.
// ---------------------------------------------------------------------------

pub struct GoToDefinition {
    pool: Arc<Pool>,
}

impl GoToDefinition {
    pub fn new(pool: Arc<Pool>) -> Self {
        Self { pool }
    }
}

#[async_trait::async_trait]
impl Tool for GoToDefinition {
    fn name(&self) -> &'static str {
        "GoToDefinition"
    }

    fn description(&self) -> &str {
        include_str!("descriptions/go_to_definition.md")
    }

    fn input_schema(&self) -> Value {
        position_schema(None)
    }

    fn meta(&self) -> ToolMeta {
        READ_ONLY
    }

    fn validate_args(&self, args_v: &Value) -> Result<(), ToolError> {
        validate_position_args("GoToDefinition", args_v, POSITION_KEYS)
    }

    async fn invoke(
        &self,
        ctx: &ToolCtx,
        args_v: Value,
    ) -> anyhow::Result<Result<ToolOutcome, ToolError>> {
        Ok(self.run(ctx, args_v).await)
    }
}

impl GoToDefinition {
    async fn run(&self, ctx: &ToolCtx, args_v: Value) -> Result<ToolOutcome, ToolError> {
        self.validate_args(&args_v)?;
        let (prepared, position) = prepare_at(&self.pool, ctx, "GoToDefinition", &args_v).await?;
        let Answer {
            value,
            readiness,
            health,
        } = prepared
            .client
            .request(
                "textDocument/definition",
                text_document_position(&prepared.file, position),
            )
            .await?;
        Ok(render::locations(
            &prepared.root,
            prepared.client.server(),
            readiness,
            health.as_deref(),
            "definitions",
            render::parse_locations(&value),
        ))
    }
}

// endregion: GoToDefinition

// region: Hover
// ---------------------------------------------------------------------------
// Hover
//
// The type and the doc comment. The cheapest of the four and the one that most
// often replaces reading a file.
// ---------------------------------------------------------------------------

pub struct Hover {
    pool: Arc<Pool>,
}

impl Hover {
    pub fn new(pool: Arc<Pool>) -> Self {
        Self { pool }
    }
}

#[async_trait::async_trait]
impl Tool for Hover {
    fn name(&self) -> &'static str {
        "Hover"
    }

    fn description(&self) -> &str {
        include_str!("descriptions/hover.md")
    }

    fn input_schema(&self) -> Value {
        position_schema(None)
    }

    fn meta(&self) -> ToolMeta {
        READ_ONLY
    }

    fn validate_args(&self, args_v: &Value) -> Result<(), ToolError> {
        validate_position_args("Hover", args_v, POSITION_KEYS)
    }

    async fn invoke(
        &self,
        ctx: &ToolCtx,
        args_v: Value,
    ) -> anyhow::Result<Result<ToolOutcome, ToolError>> {
        Ok(self.run(ctx, args_v).await)
    }
}

impl Hover {
    async fn run(&self, ctx: &ToolCtx, args_v: Value) -> Result<ToolOutcome, ToolError> {
        self.validate_args(&args_v)?;
        let (prepared, position) = prepare_at(&self.pool, ctx, "Hover", &args_v).await?;
        let Answer {
            value,
            readiness,
            health,
        } = prepared
            .client
            .request(
                "textDocument/hover",
                text_document_position(&prepared.file, position),
            )
            .await?;

        let header = render::header(prepared.client.server(), readiness, health.as_deref());
        let body = match render::hover_text(&value) {
            Some(text) => text,
            // Emptiness is a result — but only a meaningful one when the index
            // was complete, so the same rule as everywhere else applies and the
            // same function decides the sentence.
            None => render::no_results_line("type information", readiness),
        };
        Ok(ToolOutcome::new(format!("{header}\n{body}")))
    }
}

// endregion: Hover

// region: DocumentSymbols
// ---------------------------------------------------------------------------
// DocumentSymbols
//
// The shape of a file without reading it. The only one of the four that needs
// no position, which is why it is also the one to reach for first in an
// unfamiliar file.
// ---------------------------------------------------------------------------

pub struct DocumentSymbols {
    pool: Arc<Pool>,
}

impl DocumentSymbols {
    pub fn new(pool: Arc<Pool>) -> Self {
        Self { pool }
    }
}

#[async_trait::async_trait]
impl Tool for DocumentSymbols {
    fn name(&self) -> &'static str {
        "DocumentSymbols"
    }

    fn description(&self) -> &str {
        include_str!("descriptions/document_symbols.md")
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "file_path": { "type": "string", "description": "A Rust file inside the working directory." }
            },
            "required": ["file_path"],
            "additionalProperties": false,
        })
    }

    fn meta(&self) -> ToolMeta {
        READ_ONLY
    }

    fn validate_args(&self, args_v: &Value) -> Result<(), ToolError> {
        args::deny_unknown(args_v, "DocumentSymbols", &["file_path"])?;
        args::req_str(args_v, "DocumentSymbols", "file_path")?;
        Ok(())
    }

    async fn invoke(
        &self,
        ctx: &ToolCtx,
        args_v: Value,
    ) -> anyhow::Result<Result<ToolOutcome, ToolError>> {
        Ok(self.run(ctx, args_v).await)
    }
}

impl DocumentSymbols {
    async fn run(&self, ctx: &ToolCtx, args_v: Value) -> Result<ToolOutcome, ToolError> {
        self.validate_args(&args_v)?;
        let prepared = prepare(&self.pool, ctx, "DocumentSymbols", &args_v).await?;
        let Answer {
            value,
            readiness,
            health,
        } = prepared
            .client
            .request(
                "textDocument/documentSymbol",
                json!({ "textDocument": { "uri": doc::to_uri(&prepared.file) } }),
            )
            .await?;
        Ok(render::symbols(
            prepared.client.server(),
            readiness,
            health.as_deref(),
            &value,
        ))
    }
}

// endregion: DocumentSymbols
