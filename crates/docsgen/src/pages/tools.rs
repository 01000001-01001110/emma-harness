//! Diagrams for the Tools chapter of `docs/`.
//!
//! Pages: tools-ctx, tools-edit, tools-tasks, tools-trait, tools-truncation, tools-web.
//!
//! One function per diagram, each naming the source file it reads and failing
//! if that file or the item in it is gone. `super::tools_lsp` is the worked
//! example; the rules for adding one are in this module's parent.

#![allow(unused_imports)]

use std::path::Path;

use anyhow::{bail, Context, Result};

use crate::rust::{self, Item};
use crate::shapes;
use crate::svg::Diagram;

/// The gates an addressed edit passes through before writing.
///
/// Read from `ReadState::Never`, `Address`, `ReadTracker::contains_run`, and
/// `MAX_LINE_CHARS` — the four facts `by_address` consults in order.
pub fn tools_edit(root: &Path) -> Result<Diagram> {
    let session_path = root.join("tools/fs/src/session.rs");
    let session = rust::parse(&session_path)?;
    let states = rust::enum_variants(&session, "ReadState")
        .with_context(|| format!("{} declares read states", session_path.display()))?;
    let never = states
        .iter()
        .find(|s| s.name == "Never")
        .with_context(|| "ReadState::Never is the gate that blocks an unread file")?;

    let hashline_path = root.join("tools/fs/src/hashline.rs");
    let hashline = rust::parse(&hashline_path)?;
    let address_fields = rust::struct_fields(&hashline, "Address")
        .with_context(|| format!("{} declares Address", hashline_path.display()))?;

    if !rust::has_fn(&session, "contains_run") {
        bail!(
            "{} no longer declares `contains_run`",
            session_path.display()
        );
    }

    let read_path = root.join("tools/fs/src/read.rs");
    let read = rust::parse(&read_path)?;
    let max = rust::const_value(&read, "MAX_LINE_CHARS")
        .with_context(|| format!("{} declares MAX_LINE_CHARS", read_path.display()))?;

    let edit_path = root.join("tools/fs/src/edit.rs");
    let edit = rust::parse(&edit_path)?;
    if !rust::has_fn(&edit, "by_address") {
        bail!("{} no longer declares `by_address`", edit_path.display());
    }

    let n_addr = address_fields.len();
    let steps = vec![
        never.clone(),
        Item {
            name: "Address".into(),
            doc: String::new(),
            detail: format!("{n_addr} fields"),
        },
        Item {
            name: "contains_run".into(),
            doc: String::new(),
            detail: String::new(),
        },
        Item {
            name: "MAX_LINE_CHARS".into(),
            doc: String::new(),
            detail: max.clone(),
        },
    ];

    Ok(shapes::ladder(
        "tools-edit",
        format!(
            "An addressed edit passes {n} gates before the write: read state must not be \
             {never_name} ({never_doc}), the {n_addr} fields of Address must still match, \
             contains_run must see the whole run, and no line may exceed MAX_LINE_CHARS \
             ({max}). Stale is allowed through.",
            n = steps.len(),
            never_name = never.name,
            never_doc = never.doc.to_lowercase(),
            max = max,
        ),
        &steps,
        Some("Address"),
    ))
}

/// Byte offsets and edit flags on a parsed task line.
///
/// From `TaskLine`'s three offset fields and `Edit`'s three booleans in
/// `tools/tasks/src/doc.rs`.
pub fn tools_tasks(root: &Path) -> Result<Diagram> {
    let path = root.join("tools/tasks/src/doc.rs");
    let file = rust::parse(&path)?;
    let line = rust::struct_fields(&file, "TaskLine")
        .with_context(|| format!("{} declares TaskLine", path.display()))?;
    let edit = rust::struct_fields(&file, "Edit")
        .with_context(|| format!("{} declares Edit", path.display()))?;

    let offset_names = ["glyph_at", "text_end", "handle_at"];
    let mut offsets = Vec::new();
    for name in offset_names {
        offsets.push(
            line.iter()
                .find(|f| f.name == name)
                .cloned()
                .with_context(|| format!("TaskLine.{name} is in the diagram's source"))?,
        );
    }

    let edit_names = ["status", "handle", "text"];
    let mut edits = Vec::new();
    for name in edit_names {
        edits.push(
            edit.iter()
                .find(|f| f.name == name)
                .cloned()
                .with_context(|| format!("Edit.{name} is in the diagram's source"))?,
        );
    }

    let n_off = offsets.len();
    let n_edit = edits.len();
    let mut items = offsets;
    items.extend(edits);

    Ok(shapes::set(
        "tools-tasks",
        format!(
            "A parsed task line keeps {n_off} byte offsets into its original ({offsets}) and \
             {n_edit} edit flags ({edits}). A status change splices one byte; a handle \
             splices a range; new words re-emit the line canonically.",
            offsets = offset_names.join(", "),
            edits = edit_names.join(", "),
        ),
        &items,
        3,
        Some("text"),
    ))
}

/// Where one truncation reason lands on `ToolOutcome`.
///
/// `truncated_because` must exist; the diagram fans out to `content` and
/// `truncation`, the two fields it fills from one string.
pub fn tools_truncation(root: &Path) -> Result<Diagram> {
    let path = root.join("crates/tool-api/src/lib.rs");
    let file = rust::parse(&path)?;
    if !rust::has_fn(&file, "truncated_because") {
        bail!("{} no longer declares `truncated_because`", path.display());
    }
    let fields = rust::struct_fields(&file, "ToolOutcome")
        .with_context(|| format!("{} declares ToolOutcome", path.display()))?;
    let content = fields
        .iter()
        .find(|f| f.name == "content")
        .with_context(|| "ToolOutcome.content receives the model-facing cut")?;
    let truncation = fields
        .iter()
        .find(|f| f.name == "truncation")
        .with_context(|| "ToolOutcome.truncation is what the runtime quotes verbatim")?;

    let from = Item {
        name: "truncated_because".into(),
        doc: String::new(),
        detail: String::new(),
    };

    Ok(shapes::fan(
        "tools-truncation",
        format!(
            "One reason string from `truncated_because` reaches {} and {}, the two \
             ToolOutcome fields the runtime passes on verbatim.",
            content.name, truncation.name,
        ),
        &from,
        &[content.clone(), truncation.clone()],
        &[],
    ))
}

/// What every browser verb runs inside, and where the pool learns the host.
///
/// Each step is a function the source names: `Live::connect`, `Live::finish`,
/// `BrowserPool::arrived_at`, and `network_target`.
pub fn tools_web(root: &Path) -> Result<Diagram> {
    let tools_path = root.join("tools/web/src/browser/tools.rs");
    let tools = rust::parse(&tools_path)?;
    for name in ["connect", "finish", "network_target"] {
        if !rust::has_fn(&tools, name) {
            bail!("{} no longer declares `{name}`", tools_path.display());
        }
    }

    let pool_path = root.join("tools/web/src/browser/pool.rs");
    let pool = rust::parse(&pool_path)?;
    if !rust::has_fn(&pool, "arrived_at") {
        bail!("{} no longer declares `arrived_at`", pool_path.display());
    }

    let steps = vec![
        fn_item("connect"),
        fn_item("finish"),
        fn_item("arrived_at"),
        fn_item("network_target"),
    ];

    Ok(shapes::ladder(
        "tools-web",
        format!(
            "Every browser verb runs through {} steps ending at network_target: connect, \
             finish (which reads the page URL), arrived_at on the pool, then the next call's \
             host question.",
            steps.len(),
        ),
        &steps,
        Some("finish"),
    ))
}

fn fn_item(name: &str) -> Item {
    Item {
        name: name.into(),
        doc: String::new(),
        detail: String::new(),
    }
}

type BuilderSpec = (
    &'static str,
    &'static str,
    Option<(&'static str, &'static str)>,
);

const TOOL_BUILDERS: [BuilderSpec; 4] = [
    (
        "tools/fs/src/lib.rs",
        "fs_tools",
        Some(("tools/fs/src/session.rs", "ReadTracker")),
    ),
    (
        "tools/lsp/src/lib.rs",
        "lsp_tools",
        Some(("tools/lsp/src/pool.rs", "Pool")),
    ),
    (
        "tools/web/src/browser/mod.rs",
        "browser_tools",
        Some(("tools/web/src/browser/pool.rs", "BrowserPool")),
    ),
    ("tools/tasks/src/lib.rs", "task_tools", None),
];

/// Each crate's builder and the shared object it hands every member.
///
/// The builder must exist in its `lib.rs`; when it shares state, the struct
/// must exist in the file named beside it. `task_tools` is the row with no
/// shared struct — the markdown file is the state.
pub fn tools_ctx(root: &Path) -> Result<Diagram> {
    let mut steps = Vec::new();
    for (lib, builder, shared) in TOOL_BUILDERS {
        let lib_path = root.join(lib);
        let file = rust::parse(&lib_path)?;
        if !rust::has_fn(&file, builder) {
            bail!("{} no longer declares `{builder}`", lib_path.display());
        }
        let detail = if let Some((shared_path, struct_name)) = shared {
            let path = root.join(shared_path);
            rust::struct_fields(&rust::parse(&path)?, struct_name).with_context(|| {
                format!("{} declares the object `{builder}` shares", path.display())
            })?;
            format!("Arc<{struct_name}>")
        } else {
            "no shared object".into()
        };
        steps.push(Item {
            name: builder.into(),
            doc: String::new(),
            detail,
        });
    }

    let shared = steps
        .iter()
        .filter(|s| s.detail.starts_with("Arc<"))
        .count();
    let n = steps.len();
    let alone = n - shared;
    Ok(shapes::ladder(
        "tools-ctx",
        format!(
            "{shared} of the {n} tool builders wire every member to one shared \
             object; the other {alone} re-reads the file on every call. Drawn from \
             the four `*_tools` functions and the structs they Arc."
        ),
        &steps,
        Some("fs_tools"),
    ))
}

/// The checks `run_tool_call` runs before `invoke`, in order.
///
/// `fn_guards` sees only three of the five early exits — the registry miss is a
/// `let … else` and the approval denial is a `match` arm — so the ladder names
/// the six functions and fields the path actually calls, verified in the files
/// that declare them.
pub fn tools_trait(root: &Path) -> Result<Diagram> {
    let agent_path = root.join("crates/emma/src/agent.rs");
    let agent = rust::parse(&agent_path)?;
    if !rust::has_fn(&agent, "run_tool_call") {
        bail!(
            "{} no longer declares `run_tool_call`",
            agent_path.display()
        );
    }
    if !rust::has_fn(&agent, "memo_key") {
        bail!("{} no longer declares `memo_key`", agent_path.display());
    }

    let api_path = root.join("crates/tool-api/src/lib.rs");
    let api = rust::parse(&api_path)?;
    for name in ["get", "validate_args", "invoke"] {
        if !rust::has_fn(&api, name) {
            bail!("{} no longer declares `{name}`", api_path.display());
        }
    }

    let harness_path = root.join("crates/harness/src/lib.rs");
    let harness = rust::parse(&harness_path)?;
    if !rust::has_fn(&harness, "run_hooks") {
        bail!("{} no longer declares `run_hooks`", harness_path.display());
    }

    let approval_path = root.join("crates/emma/src/approval.rs");
    let approval = rust::parse(&approval_path)?;
    if !rust::has_fn(&approval, "request") {
        bail!("{} no longer declares `request`", approval_path.display());
    }

    let steps = vec![
        Item {
            name: "Registry::get".into(),
            doc: String::new(),
            detail: "miss → no_such_tool".into(),
        },
        fn_item("validate_args"),
        fn_item("memo_key"),
        Item {
            name: "PreToolUse".into(),
            doc: String::new(),
            detail: "run_hooks".into(),
        },
        Item {
            name: "approval gate".into(),
            doc: String::new(),
            detail: "request".into(),
        },
        fn_item("invoke"),
    ];

    let n = steps.len();
    let pre_invoke = n - 1;
    Ok(shapes::ladder(
        "tools-trait",
        format!(
            "{pre_invoke} checks in run_tool_call can end a call before invoke \
             runs; the ladder names all {n} stages through invoke itself. \
             Drawn from crates/emma/src/agent.rs."
        ),
        &steps,
        Some("approval gate"),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .canonicalize()
            .expect("the workspace root")
    }

    /// **If this breaks:** the edit page shows gates the addressed path no
    /// longer passes through — a renamed check keeps its box, or a new one
    /// never appears.
    #[test]
    fn the_edit_diagram_draws_the_gates_the_source_declares() {
        let root = root();
        let session = rust::parse(&root.join("tools/fs/src/session.rs")).expect("session.rs");
        let states = rust::enum_variants(&session, "ReadState").expect("ReadState");
        let never = states
            .iter()
            .find(|s| s.name == "Never")
            .expect("Never is the gate");

        let hashline = rust::parse(&root.join("tools/fs/src/hashline.rs")).expect("hashline.rs");
        let address = rust::struct_fields(&hashline, "Address").expect("Address");

        let read = rust::parse(&root.join("tools/fs/src/read.rs")).expect("read.rs");
        let max = rust::const_value(&read, "MAX_LINE_CHARS").expect("MAX_LINE_CHARS");

        let d = tools_edit(&root).expect("the diagram builds");
        let drawn: Vec<String> = d.layers.iter().flatten().map(|n| n.id.clone()).collect();
        assert_eq!(drawn.len(), 4, "{drawn:?}");
        assert!(drawn.contains(&never.name), "{drawn:?}");
        assert!(drawn.contains(&"Address".to_string()), "{drawn:?}");
        assert!(drawn.contains(&"contains_run".to_string()), "{drawn:?}");
        assert!(drawn.contains(&"MAX_LINE_CHARS".to_string()), "{drawn:?}");
        assert!(d.caption.contains(&never.name), "{}", d.caption);
        assert!(d.caption.contains(&max), "{}", d.caption);
        assert_eq!(address.len(), 2, "Address has start and end");
    }

    /// **If this breaks:** a renamed session file still produces a diagram.
    #[test]
    fn a_missing_source_fails_the_edit_diagram_rather_than_emptying_it() {
        let Err(e) = tools_edit(Path::new("definitely-not-a-workspace")) else {
            panic!("a missing tree must not produce a diagram");
        };
        let msg = format!("{e:#}");
        assert!(msg.contains("session.rs"), "{msg}");
    }

    /// **If this breaks:** the tasks page shows offsets or edit flags the
    /// structs no longer carry.
    #[test]
    fn the_tasks_diagram_draws_the_struct_fields_the_parser_declares() {
        let root = root();
        let file = rust::parse(&root.join("tools/tasks/src/doc.rs")).expect("doc.rs");
        let line = rust::struct_fields(&file, "TaskLine").expect("TaskLine");
        let edit = rust::struct_fields(&file, "Edit").expect("Edit");

        for name in ["glyph_at", "text_end", "handle_at"] {
            assert!(line.iter().any(|f| f.name == name), "{name} in TaskLine");
        }
        for name in ["status", "handle", "text"] {
            assert!(edit.iter().any(|f| f.name == name), "{name} in Edit");
        }

        let d = tools_tasks(&root).expect("the diagram builds");
        let drawn: Vec<String> = d.layers.iter().flatten().map(|n| n.id.clone()).collect();
        assert_eq!(drawn.len(), 6, "{drawn:?}");
        for name in [
            "glyph_at",
            "text_end",
            "handle_at",
            "status",
            "handle",
            "text",
        ] {
            assert!(
                drawn.contains(&name.to_string()),
                "{name} missing: {drawn:?}"
            );
        }
        assert!(
            d.caption.contains(&line.len().to_string()) || d.caption.contains('3'),
            "the caption states the offset count: {}",
            d.caption
        );
    }

    /// **If this breaks:** a renamed doc file still produces a diagram.
    #[test]
    fn a_missing_source_fails_the_tasks_diagram_rather_than_emptying_it() {
        let Err(e) = tools_tasks(Path::new("definitely-not-a-workspace")) else {
            panic!("a missing tree must not produce a diagram");
        };
        let msg = format!("{e:#}");
        assert!(msg.contains("doc.rs"), "{msg}");
    }

    /// **If this breaks:** the truncation page fans out to fields the outcome
    /// struct no longer has.
    #[test]
    fn the_truncation_diagram_fans_to_the_two_fields_tooloutcome_declares() {
        let root = root();
        let file = rust::parse(&root.join("crates/tool-api/src/lib.rs")).expect("lib.rs");
        assert!(rust::has_fn(&file, "truncated_because"));
        let fields = rust::struct_fields(&file, "ToolOutcome").expect("ToolOutcome");

        let d = tools_truncation(&root).expect("the diagram builds");
        assert_eq!(d.layers.len(), 2, "one head, one tail row");
        assert_eq!(d.layers[0][0].id, "truncated_because");
        let tail: Vec<String> = d.layers[1].iter().map(|n| n.id.clone()).collect();
        for name in ["content", "truncation"] {
            assert!(
                fields.iter().any(|f| f.name == name),
                "{name} in ToolOutcome"
            );
            assert!(tail.contains(&name.to_string()), "{name} missing: {tail:?}");
        }
        assert_eq!(d.edges.len(), 2, "one edge per destination field");
    }

    /// **If this breaks:** a renamed tool-api file still produces a diagram.
    #[test]
    fn a_missing_source_fails_the_truncation_diagram_rather_than_emptying_it() {
        let Err(e) = tools_truncation(Path::new("definitely-not-a-workspace")) else {
            panic!("a missing tree must not produce a diagram");
        };
        let msg = format!("{e:#}");
        assert!(msg.contains("tool-api"), "{msg}");
    }

    /// **If this breaks:** the web page shows browser steps the source no
    /// longer names.
    #[test]
    fn the_web_diagram_draws_the_live_and_pool_steps_the_source_declares() {
        let root = root();
        let tools = rust::parse(&root.join("tools/web/src/browser/tools.rs")).expect("tools.rs");
        for name in ["connect", "finish", "network_target"] {
            assert!(rust::has_fn(&tools, name), "{name} in tools.rs");
        }
        let pool = rust::parse(&root.join("tools/web/src/browser/pool.rs")).expect("pool.rs");
        assert!(rust::has_fn(&pool, "arrived_at"));

        let d = tools_web(&root).expect("the diagram builds");
        let drawn: Vec<String> = d.layers.iter().flatten().map(|n| n.id.clone()).collect();
        assert_eq!(drawn.len(), 4, "{drawn:?}");
        for name in ["connect", "finish", "arrived_at", "network_target"] {
            assert!(
                drawn.contains(&name.to_string()),
                "{name} missing: {drawn:?}"
            );
        }
        assert!(
            d.caption.contains(&drawn.len().to_string()),
            "the caption states the step count: {}",
            d.caption
        );
    }

    /// **If this breaks:** a renamed browser file still produces a diagram.
    #[test]
    fn a_missing_source_fails_the_web_diagram_rather_than_emptying_it() {
        let Err(e) = tools_web(Path::new("definitely-not-a-workspace")) else {
            panic!("a missing tree must not produce a diagram");
        };
        let msg = format!("{e:#}");
        assert!(msg.contains("tools.rs") || msg.contains("pool.rs"), "{msg}");
    }

    /// **If this breaks:** the ToolCtx page shows a builder or shared struct
    /// the source no longer declares.
    #[test]
    fn the_ctx_diagram_draws_the_builders_and_shared_structs_the_source_declares() {
        let root = root();
        for (lib, builder, shared) in TOOL_BUILDERS {
            let file = rust::parse(&root.join(lib)).expect("{lib} parses");
            assert!(rust::has_fn(&file, builder), "{builder} in {lib}");
            if let Some((shared_path, struct_name)) = shared {
                let shared_file = rust::parse(&root.join(shared_path)).expect("shared file");
                rust::struct_fields(&shared_file, struct_name).expect(struct_name);
            }
        }

        let d = tools_ctx(&root).expect("the diagram builds");
        let drawn: Vec<String> = d.layers.iter().flatten().map(|n| n.id.clone()).collect();
        assert_eq!(drawn.len(), TOOL_BUILDERS.len(), "{drawn:?}");
        for (_, builder, _) in TOOL_BUILDERS {
            assert!(
                drawn.contains(&builder.to_string()),
                "{builder} missing: {drawn:?}"
            );
        }
        let shared = TOOL_BUILDERS.iter().filter(|(_, _, s)| s.is_some()).count();
        assert!(
            d.caption.contains(&shared.to_string()),
            "the caption states how many share: {}",
            d.caption
        );
    }

    /// **If this breaks:** a renamed builder file still produces a diagram.
    #[test]
    fn a_missing_source_fails_the_ctx_diagram_rather_than_emptying_it() {
        let Err(e) = tools_ctx(Path::new("definitely-not-a-workspace")) else {
            panic!("a missing tree must not produce a diagram");
        };
        let msg = format!("{e:#}");
        assert!(msg.contains("lib.rs"), "{msg}");
    }

    /// **If this breaks:** the trait page shows dispatch stages the call path
    /// no longer names.
    #[test]
    fn the_trait_diagram_draws_the_stages_run_tool_call_declares() {
        let root = root();
        let agent = rust::parse(&root.join("crates/emma/src/agent.rs")).expect("agent.rs");
        assert!(rust::has_fn(&agent, "run_tool_call"));
        assert!(rust::has_fn(&agent, "memo_key"));

        let api = rust::parse(&root.join("crates/tool-api/src/lib.rs")).expect("tool-api");
        for name in ["get", "validate_args", "invoke"] {
            assert!(rust::has_fn(&api, name), "{name} in tool-api");
        }

        let harness = rust::parse(&root.join("crates/harness/src/lib.rs")).expect("harness");
        assert!(rust::has_fn(&harness, "run_hooks"));

        let approval = rust::parse(&root.join("crates/emma/src/approval.rs")).expect("approval");
        assert!(rust::has_fn(&approval, "request"));

        let d = tools_trait(&root).expect("the diagram builds");
        let drawn: Vec<String> = d.layers.iter().flatten().map(|n| n.id.clone()).collect();
        assert_eq!(drawn.len(), 6, "{drawn:?}");
        for name in [
            "Registry::get",
            "validate_args",
            "memo_key",
            "PreToolUse",
            "approval gate",
            "invoke",
        ] {
            assert!(
                drawn.contains(&name.to_string()),
                "{name} missing: {drawn:?}"
            );
        }
        assert!(
            d.caption.contains('5'),
            "the caption states how many can end early: {}",
            d.caption
        );
    }

    /// **If this breaks:** a renamed agent file still produces a diagram.
    #[test]
    fn a_missing_source_fails_the_trait_diagram_rather_than_emptying_it() {
        let Err(e) = tools_trait(Path::new("definitely-not-a-workspace")) else {
            panic!("a missing tree must not produce a diagram");
        };
        let msg = format!("{e:#}");
        assert!(msg.contains("agent.rs"), "{msg}");
    }
}
