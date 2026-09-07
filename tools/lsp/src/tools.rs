//! The seven tools, and the two paths a position-based question takes.
//!
//! Each tool is a thin shell — validate, contain, locate, ask, render — because
//! everything hard is in the other modules and repeating any of it here is how
//! seven tools come to disagree about containment.
//!
//! **The shared shape.** `FindReferences`, `GoToDefinition` and `Hover` take the
//! same three arguments: a file, a 1-based line, and the symbol's name. See
//! [`crate::doc::locate`] for why a name rather than a column — asking a model
//! for a column offset gets a plausible number, and a plausible number is a
//! wrong answer about the symbol next door. The shape also means `Grep` output
//! feeds straight in, which is the actual workflow: grep to find candidate
//! lines, then ask the compiler which of them is real.
//!
//! **And the second shape, for the two that ask about a *point*.**
//! `Completion` and `SignatureHelp` take `after` instead of `symbol`: the text
//! immediately before the place being asked about, with the cursor put at its
//! end. A completion after a dot has no symbol to name — `config.` is not an
//! identifier and the line does not compile yet — so naming a symbol would be
//! the wrong question. The same substring search backs both, so the same three
//! refusals apply.
//!
//! **What every one of them does before asking.** Resolve the path through
//! `tools/fs`'s containment, find the language for the file, read it
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
use crate::lang;
use crate::pool::Pool;
use crate::render;

// region: The shared preamble
// ---------------------------------------------------------------------------
// The shared preamble
//
// Containment, the language gate, the read, the position, and the sync — in one
// place so that seven tools cannot each get a different part of it wrong.
// ---------------------------------------------------------------------------

/// Everything a tool needs after the preamble has run.
struct Prepared {
    client: Arc<Client>,
    root: PathBuf,
    file: PathBuf,
    text: String,
}

/// Contain the path, find the language, read the file, and get a server that
/// knows about it.
///
/// Note the order. Containment first, before the file is read and before a
/// server is started: a path outside the root must not so much as cause a
/// process to spawn. The language gate is second, and it is three refusals
/// rather than one, because "no server for this" and "there is one and it is
/// switched off" and "this is YAML that is not Ansible" have three different
/// fixes.
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
    let shown = path::display(&root, &file);
    let language = match lang::for_path(&root, &file) {
        Some(l) => l,
        None if is_yaml(&file) => {
            return Err(ToolError::Unavailable(lang::yaml_not_ansible(&shown)))
        }
        None => return Err(crate::server::unsupported_language(&shown)),
    };

    let text = std::fs::read_to_string(&file)
        .map_err(|e| ToolError::Failed(format!("{raw} could not be read: {e}")))?;

    let client = pool.client(&root, language).await?;
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

/// The preamble plus a cursor *after* a named piece of text.
///
/// **Completion and signature help are questions about a point, not about a
/// symbol**, and the difference is the whole reason this exists beside
/// [`prepare_at`]. `config.` has no symbol to name -- the useful position is one
/// character past a dot, on a line that does not compile yet. So the argument is
/// the text immediately before the point, and the cursor goes at its end.
///
/// [`doc::locate`] does a plain substring search, which is what makes this work
/// for `config.` and `take(1, ` as well as for a bare name, and its refusals --
/// not on that line, ambiguous without `occurrence` -- are the ones already
/// written and already understood.
async fn prepare_after(
    pool: &Pool,
    ctx: &ToolCtx,
    tool: &str,
    args_v: &Value,
) -> Result<(Prepared, Position), ToolError> {
    let line = args::req_u64(args_v, tool, "line")?;
    let after = args::req_str(args_v, tool, "after")?.to_string();
    let occurrence = args::opt_u64(args_v, tool, "occurrence")?;

    let prepared = prepare(pool, ctx, tool, args_v).await?;
    let (start, _) = doc::locate(&prepared.text, line, &after, occurrence)?;
    // Past the end of the match, in the same UTF-16 units the position is in.
    // Counting the argument's own units rather than re-measuring the line,
    // because the two agree by construction and only one of them is in hand —
    // and counting them through [`doc::utf16_column`] rather than inline,
    // because this crate has exactly one answer to "how wide is that in the
    // protocol's units" and a second one here would be a second answer.
    let width = doc::utf16_column(&after, after.len());
    Ok((
        prepared,
        Position {
            line: start.line,
            character: start.character + width,
        },
    ))
}

/// The one sentence every schema here says about `file_path`.
///
/// All seven tools take the same argument and it means the same thing in each,
/// so it is written once: four copies of a sentence the model learns the tool
/// surface from is four chances for one of them to say something else.
const FILE_PATH_DESCRIPTION: &str =
    "A source file inside the working directory. Emma picks the language server from the \
     extension.";

/// The arguments the two point tools share, and their schema.
const CURSOR_KEYS: &[&str] = &["file_path", "line", "after", "occurrence"];

fn cursor_schema(after: &str) -> Value {
    json!({
        "type": "object",
        "properties": {
            "file_path": { "type": "string", "description": FILE_PATH_DESCRIPTION },
            "line": { "type": "integer", "minimum": 1, "description": "1-based line number the point is on." },
            "after": { "type": "string", "description": after },
            "occurrence": { "type": "integer", "minimum": 1, "description": "Which occurrence on the line, 1-based. Only needed when the text appears more than once." }
        },
        "required": ["file_path", "line", "after"],
        "additionalProperties": false,
    })
}

fn validate_cursor_args(tool: &str, args_v: &Value) -> Result<(), ToolError> {
    args::deny_unknown(args_v, tool, CURSOR_KEYS)?;
    args::req_str(args_v, tool, "file_path")?;
    let line = args::req_u64(args_v, tool, "line")?;
    if line == 0 {
        return Err(ToolError::BadArguments(format!(
            "{tool}.line is 1-based; there is no line 0"
        )));
    }
    if args::req_str(args_v, tool, "after")?.is_empty() {
        // An empty string matches at column 0 of every line, which would be a
        // confident answer about the start of the line rather than about the
        // point the caller meant.
        return Err(ToolError::BadArguments(format!(
            "{tool}.after is empty; name the text immediately before the point you are asking \
             about"
        )));
    }
    if let Some(0) = args::opt_u64(args_v, tool, "occurrence")? {
        return Err(ToolError::BadArguments(format!(
            "{tool}.occurrence is 1-based; there is no occurrence 0"
        )));
    }
    Ok(())
}

/// The whole schema of the two tools that ask about a *file* rather than a
/// point in one, `DocumentSymbols` and `Diagnostics`.
///
/// One JSON blob rather than two identical ones. The description of
/// `file_path` is the same sentence in all seven schemas, and it is the sentence
/// that tells the model Emma picks the server from the extension — a copy of it
/// that drifted would teach the model something different about one tool.
fn file_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "file_path": { "type": "string", "description": FILE_PATH_DESCRIPTION }
        },
        "required": ["file_path"],
        "additionalProperties": false,
    })
}

/// What the two file tools check before anything is opened.
fn validate_file_args(tool: &str, args_v: &Value) -> Result<(), ToolError> {
    args::deny_unknown(args_v, tool, &["file_path"])?;
    args::req_str(args_v, tool, "file_path")?;
    Ok(())
}

/// Whether a path is YAML, so the Ansible refusal can be the specific one.
fn is_yaml(file: &std::path::Path) -> bool {
    file.extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .is_some_and(|e| e == "yml" || e == "yaml")
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
        "file_path": { "type": "string", "description": FILE_PATH_DESCRIPTION },
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
/// and proc macros, which are the three ways rust-analyzer runs the analysed
/// project's code or fills its build directory. The one file the server still
/// writes, `Cargo.lock`, is measured and argued in `tests/real_server.rs` —
/// together with why it stays inside the bit. The fake-server suite
/// (`tests/tools.rs`) proves the tools themselves write nothing.
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

/// The shell every tool in this file is: a struct holding the pool, a
/// constructor, and the five `Tool` methods.
///
/// **Written once because it had been written seven times.** Three of those five
/// methods — `name`, `meta`, `invoke` — are identical in every tool here and two
/// of them carry decisions that must hold for all seven or for none:
/// [`READ_ONLY`]'s argument, and the ruling that a tool failure is an
/// observation rather than an abort. Seven copies is seven places for a fix to
/// be applied six times, and this crate has already paid that once, on two
/// request handlers that each dropped an error arm the shared helper had.
///
/// What stays hand-written is what actually differs, and it is not
/// boilerplate: the schema, the cheap validation, and `run`. Each `run` names
/// its own LSP method and its own renderer, which is the whole of what a tool
/// is; everything the seven share before that point is already in [`prepare`].
///
/// The tool's name is `stringify!` of the type rather than a literal because it
/// used to be a literal in three places per tool — the type, `name`, and the
/// string every error message is built from — with nothing making them agree.
macro_rules! lsp_tool {
    (
        $ty:ident,
        description: $description:literal,
        schema: $schema:expr,
        validate: $validate:expr $(,)?
    ) => {
        pub struct $ty {
            pool: Arc<Pool>,
        }

        impl $ty {
            pub fn new(pool: Arc<Pool>) -> Self {
                Self { pool }
            }
        }

        #[async_trait::async_trait]
        impl Tool for $ty {
            fn name(&self) -> &'static str {
                stringify!($ty)
            }

            fn description(&self) -> &str {
                include_str!($description)
            }

            fn input_schema(&self) -> Value {
                $schema
            }

            fn meta(&self) -> ToolMeta {
                READ_ONLY
            }

            fn validate_args(&self, args_v: &Value) -> Result<(), ToolError> {
                // Annotated as a plain function pointer so the argument cannot
                // capture: validation runs before any process exists and has
                // nothing to capture from.
                let check: fn(&str, &Value) -> Result<(), ToolError> = $validate;
                check(self.name(), args_v)
            }

            async fn invoke(
                &self,
                ctx: &ToolCtx,
                args_v: Value,
            ) -> anyhow::Result<Result<ToolOutcome, ToolError>> {
                // A tool failure is an observation, not a fault: the `Err` goes
                // back to the model as a `tool_result` and the loop continues.
                Ok(self.run(ctx, args_v).await)
            }
        }
    };
}

// endregion: The shared preamble

// region: FindReferences
// ---------------------------------------------------------------------------
// FindReferences
//
// The one that most beats grep, and the one whose empty result is most
// dangerous — which is the same fact twice.
// ---------------------------------------------------------------------------

const REFERENCES_KEYS: &[&str] = &[
    "file_path",
    "line",
    "symbol",
    "occurrence",
    "include_declaration",
];

lsp_tool! {
    FindReferences,
    description: "descriptions/find_references.md",
    schema: position_schema(Some((
        "include_declaration",
        json!({
            "type": "boolean",
            "description": "Include the definition itself among the results. Defaults to true.",
        }),
    ))),
    validate: |tool, args| validate_position_args(tool, args, REFERENCES_KEYS),
}

impl FindReferences {
    async fn run(&self, ctx: &ToolCtx, args_v: Value) -> Result<ToolOutcome, ToolError> {
        self.validate_args(&args_v)?;
        let include_declaration = match args_v.get("include_declaration") {
            None | Some(Value::Null) => true,
            Some(Value::Bool(b)) => *b,
            Some(other) => {
                return Err(ToolError::BadArguments(format!(
                    "{}.include_declaration must be true or false, got {}",
                    self.name(),
                    args::type_name(other)
                )))
            }
        };
        let (prepared, position) = prepare_at(&self.pool, ctx, self.name(), &args_v).await?;

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

lsp_tool! {
    GoToDefinition,
    description: "descriptions/go_to_definition.md",
    schema: position_schema(None),
    validate: |tool, args| validate_position_args(tool, args, POSITION_KEYS),
}

impl GoToDefinition {
    async fn run(&self, ctx: &ToolCtx, args_v: Value) -> Result<ToolOutcome, ToolError> {
        self.validate_args(&args_v)?;
        let (prepared, position) = prepare_at(&self.pool, ctx, self.name(), &args_v).await?;
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
// The type and the doc comment. The cheapest of them all and the one that most
// often replaces reading a file.
// ---------------------------------------------------------------------------

lsp_tool! {
    Hover,
    description: "descriptions/hover.md",
    schema: position_schema(None),
    validate: |tool, args| validate_position_args(tool, args, POSITION_KEYS),
}

impl Hover {
    async fn run(&self, ctx: &ToolCtx, args_v: Value) -> Result<ToolOutcome, ToolError> {
        self.validate_args(&args_v)?;
        let (prepared, position) = prepare_at(&self.pool, ctx, self.name(), &args_v).await?;
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

        Ok(render::hover(
            prepared.client.server(),
            readiness,
            health.as_deref(),
            &value,
        ))
    }
}

// endregion: Hover

// region: DocumentSymbols
// ---------------------------------------------------------------------------
// DocumentSymbols
//
// The shape of a file without reading it. One of only two that need no
// position, which is why it is also the one to reach for first in an
// unfamiliar file.
// ---------------------------------------------------------------------------

lsp_tool! {
    DocumentSymbols,
    description: "descriptions/document_symbols.md",
    schema: file_schema(),
    validate: validate_file_args,
}

impl DocumentSymbols {
    async fn run(&self, ctx: &ToolCtx, args_v: Value) -> Result<ToolOutcome, ToolError> {
        self.validate_args(&args_v)?;
        let prepared = prepare(&self.pool, ctx, self.name(), &args_v).await?;
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

// region: Completion
// ---------------------------------------------------------------------------
// Completion
//
// What can be typed here. The only tool of the seven whose answer is a *ranked*
// list, which is why it is capped far lower than the others and why its order is
// never touched.
// ---------------------------------------------------------------------------

lsp_tool! {
    Completion,
    description: "descriptions/completion.md",
    schema: cursor_schema(
        "The text on that line immediately before the point you are asking about. Include the \
         dot to ask for a value's members, as in \"config.\". The line need not compile.",
    ),
    validate: validate_cursor_args,
}

impl Completion {
    async fn run(&self, ctx: &ToolCtx, args_v: Value) -> Result<ToolOutcome, ToolError> {
        self.validate_args(&args_v)?;
        let (prepared, position) = prepare_after(&self.pool, ctx, self.name(), &args_v).await?;
        let Answer {
            value,
            readiness,
            health,
        } = prepared
            .client
            .request(
                "textDocument/completion",
                text_document_position(&prepared.file, position),
            )
            .await?;
        Ok(render::completions(
            prepared.client.server(),
            readiness,
            health.as_deref(),
            &value,
        ))
    }
}

// endregion: Completion

// region: SignatureHelp
// ---------------------------------------------------------------------------
// SignatureHelp
//
// The parameters of the call the point is inside. Distinct from `Hover` on the
// callee, because the answer here knows which argument is being typed and hover
// cannot.
// ---------------------------------------------------------------------------

lsp_tool! {
    SignatureHelp,
    description: "descriptions/signature_help.md",
    schema: cursor_schema(
        "The text on that line immediately before the point you are asking about, usually the \
         open bracket and any arguments already typed, as in \"take(1, \".",
    ),
    validate: validate_cursor_args,
}

impl SignatureHelp {
    async fn run(&self, ctx: &ToolCtx, args_v: Value) -> Result<ToolOutcome, ToolError> {
        self.validate_args(&args_v)?;
        let (prepared, position) = prepare_after(&self.pool, ctx, self.name(), &args_v).await?;
        let Answer {
            value,
            readiness,
            health,
        } = prepared
            .client
            .request(
                "textDocument/signatureHelp",
                text_document_position(&prepared.file, position),
            )
            .await?;
        Ok(render::signatures(
            prepared.client.server(),
            readiness,
            health.as_deref(),
            &value,
        ))
    }
}

// endregion: SignatureHelp

// region: Diagnostics
// ---------------------------------------------------------------------------
// Diagnostics
//
// The fifth tool, and the one the crate previously refused to ship. The refusal
// was right at the time and for a reason recorded in `lib.rs`: diagnostics are
// pushed rather than requested, so "get the diagnostics" means waiting an
// unknowable time for a notification that may never come, and for rust the
// diagnostics worth having are `cargo check`'s, which this crate turns off.
//
// Both halves of that changed. Several of the seven languages here publish on
// `didOpen` within a second and it is the only thing some of them are for, and
// the unknowable wait is solved rather than ignored: `Client::diagnostics`
// returns an `Option`, `None` is not an empty list, and `render::diagnostics`
// gives them sentences that cannot be mistaken for each other. An empty result
// means the server said clean. Silence says silence.
// ---------------------------------------------------------------------------

lsp_tool! {
    Diagnostics,
    description: "descriptions/diagnostics.md",
    schema: file_schema(),
    validate: validate_file_args,
}

impl Diagnostics {
    async fn run(&self, ctx: &ToolCtx, args_v: Value) -> Result<ToolOutcome, ToolError> {
        self.validate_args(&args_v)?;
        // `prepare` is what sends `didOpen` or `didChange`, and that is the
        // event a server publishes in response to. Asking before syncing would
        // wait for a notification nothing had asked for.
        let prepared = prepare(&self.pool, ctx, self.name(), &args_v).await?;
        let diagnosis = prepared
            .client
            .diagnostics(&doc::to_uri(&prepared.file))
            .await?;
        Ok(render::diagnostics(
            &prepared.root,
            &prepared.file,
            prepared.client.server(),
            diagnosis.readiness,
            diagnosis.health.as_deref(),
            diagnosis.waited,
            diagnosis.items,
        ))
    }
}

// endregion: Diagnostics
