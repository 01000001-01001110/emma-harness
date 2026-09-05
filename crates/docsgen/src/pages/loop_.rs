//! Diagrams for the agent loop and sessions chapter of `docs/`.
//!
//! Pages: loop-endings, loop-failure, loop-memo, loop-one-turn,
//! session-compaction, session-fold.
//!
//! One function per diagram, each naming the source file it reads and failing
//! if that file or the item in it is gone. `super::tools_lsp` is the worked
//! example; the rules for adding one are in this module's parent.

use std::path::Path;

use anyhow::{bail, Context, Result};
use syn::{Block, Expr, Stmt};

use crate::rust::{self, Item};
use crate::shapes;
use crate::svg::Diagram;

fn fn_item(name: &str) -> Item {
    Item {
        name: name.into(),
        doc: String::new(),
        detail: String::new(),
    }
}

/// The four endings the empty-tool-calls branch of `run_goal` can produce.
///
/// Looked up on `Ending`, in the order the checks run, which is not the enum's
/// declaration order (`Done`, `KicksExhausted`, `Stalled`, `Answered`, …).
/// Swapping two variants in the enum would leave a declaration-order diagram
/// looking right while the function did the other thing.
pub fn loop_endings(root: &Path) -> Result<Diagram> {
    let path = root.join("crates/emma/src/agent.rs");
    let file = rust::parse(&path)?;
    if !rust::has_fn(&file, "run_goal") {
        bail!("{} no longer declares `run_goal`", path.display());
    }
    let variants = rust::enum_variants(&file, "Ending")
        .with_context(|| format!("{} declares Ending", path.display()))?;

    let names = ["Done", "Stalled", "Answered", "KicksExhausted"];
    let mut steps = Vec::new();
    for name in names {
        steps.push(
            variants
                .iter()
                .find(|v| v.name == name)
                .cloned()
                .with_context(|| {
                    format!("Ending::{name} is a rung of the empty-tool-calls ladder")
                })?,
        );
    }

    let n = steps.len();
    let drawn: Vec<&str> = steps.iter().map(|s| s.name.as_str()).collect();
    Ok(shapes::ladder(
        "endings",
        format!(
            "{n} endings the empty-tool-calls branch of run_goal can produce, \
             in the order the checks run: {}. Falling through all {n} sends a \
             kick and continues the loop.",
            drawn.join(", ")
        ),
        &steps,
        Some("Done"),
    ))
}

/// Every `return fail(...)` in `run_tool_call`, in the order the function
/// reaches them.
///
/// The kinds are string literals where the source names them, and the method
/// call where the kind comes from `e.kind()`. A let-else and several match
/// arms sit beside the if-guards `fn_guards` would see alone; all of them
/// converge on the same closure.
pub fn loop_failure(root: &Path) -> Result<Diagram> {
    let path = root.join("crates/emma/src/agent.rs");
    let file = rust::parse(&path)?;
    if !rust::has_fn(&file, "run_tool_call") {
        bail!("{} no longer declares `run_tool_call`", path.display());
    }
    let paths = fail_paths(&file, "run_tool_call")
        .with_context(|| format!("{} declares failure exits", path.display()))?;

    let n = paths.len();
    Ok(shapes::ladder(
        "failure",
        format!(
            "{n} failure exits in run_tool_call, in source order; each returns \
             through the same fail closure, which builds a tool_result with \
             is_error set and logs it before the loop continues."
        ),
        &paths,
        Some(&paths.last().expect("at least one path").name),
    ))
}

/// One iteration of `run_goal`: the three budget guards, compaction, the four
/// request fields that change per call, the model call, and where the held
/// turn is placed.
pub fn loop_one_turn(root: &Path) -> Result<Diagram> {
    let agent = root.join("crates/emma/src/agent.rs");
    let agent_file = rust::parse(&agent)?;
    if !rust::has_fn(&agent_file, "run_goal") {
        bail!("{} no longer declares `run_goal`", agent.display());
    }
    for name in ["compact_if_needed", "call_model", "place"] {
        if !rust::has_fn(&agent_file, name) {
            bail!("{} no longer declares `{name}`", agent.display());
        }
    }

    let endings = rust::enum_variants(&agent_file, "Ending")
        .with_context(|| format!("{} declares Ending", agent.display()))?;
    let guard_names = ["Interrupted", "Deadline", "Iterations"];
    let mut steps = Vec::new();
    for name in guard_names {
        steps.push(
            endings
                .iter()
                .find(|v| v.name == name)
                .cloned()
                .with_context(|| format!("Ending::{name} is a guard on run_goal"))?,
        );
    }
    steps.push(fn_item("compact_if_needed"));

    let llm = root.join("crates/llm/src/lib.rs");
    let llm_file = rust::parse(&llm)?;
    let request_fields = rust::struct_fields(&llm_file, "Request")
        .with_context(|| format!("{} declares Request", llm.display()))?;
    let request_parts = ["instructions", "tools", "history", "query"];
    for name in request_parts {
        steps.push(
            request_fields
                .iter()
                .find(|f| f.name == name)
                .cloned()
                .with_context(|| format!("Request.{name} is built each iteration"))?,
        );
    }
    steps.push(fn_item("call_model"));
    steps.push(fn_item("place"));

    let n = steps.len();
    Ok(shapes::ladder(
        "one-turn",
        format!(
            "{n} steps in one run_goal iteration: the three budget guards \
             ({guard_names:?}), compact_if_needed, the four Request fields \
             that move each call, call_model, then place once the turn knows \
             where it goes.",
            guard_names = guard_names
        ),
        &steps,
        Some("call_model"),
    ))
}

/// The compaction decision points in `compact_if_needed` and `compact`.
///
/// Read as early-return guards in the order they are checked. When none of
/// them fire, chapters are taken oldest-first until the remainder fits half
/// the cap; the arithmetic between the guards is not a separate rung.
pub fn session_compaction(root: &Path) -> Result<Diagram> {
    let path = root.join("crates/emma/src/agent.rs");
    let file = rust::parse(&path)?;
    for name in ["compact_if_needed", "compact"] {
        if !rust::has_fn(&file, name) {
            bail!("{} no longer declares `{name}`", path.display());
        }
    }
    let mut steps = rust::fn_guards(&file, "compact_if_needed")
        .with_context(|| format!("{} declares the compaction trigger", path.display()))?;
    steps.extend(
        rust::fn_guards(&file, "compact")
            .with_context(|| format!("{} declares the abandon checks", path.display()))?,
    );

    let n = steps.len();
    Ok(shapes::ladder(
        "compaction",
        format!(
            "{n} early-return checks across compact_if_needed and compact, in \
             the order they run. The two the page highlights are size > cap \
             (the second guard, inverted) and after >= before (the last)."
        ),
        &steps,
        Some(&steps[steps.len() - 1].name),
    ))
}

/// The memo as the writer, the set, and the reader.
///
/// `failed_now` is a local in `run_goal` and a field on `Resumed`; the field is
/// the item a machine can name. `memo_key` is required to exist because both
/// sides and a resume share it, but it is not a box — a fourth box would invent
/// a step the picture does not have.
pub fn loop_memo(root: &Path) -> Result<Diagram> {
    let path = root.join("crates/emma/src/agent.rs");
    let file = rust::parse(&path)?;
    for name in ["run_goal", "run_tool_call", "memo_key"] {
        if !rust::has_fn(&file, name) {
            bail!("{} no longer declares `{name}`", path.display());
        }
    }
    let fields = rust::struct_fields(&file, "Resumed")
        .with_context(|| format!("{} declares Resumed", path.display()))?;
    let failed_now = fields
        .iter()
        .find(|f| f.name == "failed_now")
        .cloned()
        .with_context(|| "Resumed.failed_now is the memo the loop restores")?;

    let steps = vec![fn_item("run_goal"), failed_now, fn_item("run_tool_call")];
    let n = steps.len();
    Ok(shapes::ladder(
        "memo",
        format!(
            "The memo is {n} named pieces: run_goal writes it, failed_now holds \
             it, run_tool_call only reads it. Clearing lives in the writer, so \
             a reader of run_tool_call alone sees a permanent blacklist."
        ),
        &steps,
        Some("failed_now"),
    ))
}

/// The state a fold walk keeps, from the struct itself.
///
/// The hand-drawn picture was the close_turn / answered / close_goal flow.
/// That flow is an argument about when half a turn is worse than none of it,
/// and it belongs in the quoted methods. The heading above the figure is
/// "The state a walk needs", which is `Fold`'s fields.
pub fn session_fold(root: &Path) -> Result<Diagram> {
    let path = root.join("crates/emma/src/session.rs");
    let file = rust::parse(&path)?;
    for name in ["fold", "fold_records", "close_turn", "place_turn"] {
        if !rust::has_fn(&file, name) {
            bail!("{} no longer declares `{name}`", path.display());
        }
    }
    let fields = rust::struct_fields(&file, "Fold")
        .with_context(|| format!("{} declares Fold", path.display()))?;

    let n = fields.len();
    let names: Vec<&str> = fields.iter().map(|f| f.name.as_str()).collect();
    Ok(shapes::set(
        "fold",
        format!(
            "The {n} fields of Fold: {}. Two lists because the loop keeps two; \
             damage is the records the fold refused, so a resume can say the \
             conversation it rebuilt is known not to match.",
            names.join(", ")
        ),
        &fields,
        4,
        Some("pending"),
    ))
}

/// Each `return fail(...)` in a named function, in source order.
fn fail_paths(file: &syn::File, name: &str) -> Result<Vec<Item>> {
    let stmts = fn_body(file, name).with_context(|| format!("no `fn {name}` in this file"))?;
    let mut paths = Vec::new();
    FailPathVisitor::new(&mut paths).walk_stmts(&stmts);
    merge_select_fails(&stmts, &mut paths);
    if paths.is_empty() {
        bail!("`fn {name}` has no `return fail(...)` paths to draw");
    }
    Ok(paths)
}

/// `tokio::select!` arms are a macro, not Rust the parser can walk. Scan the
/// token tree for `fail("…")` literals the visitor missed and insert them
/// before the invoke paths that follow the macro in the function.
fn merge_select_fails(stmts: &[Stmt], paths: &mut Vec<Item>) {
    for stmt in stmts {
        let Some(mac) = select_macro_in_stmt(stmt) else {
            continue;
        };
        for kind in fail_literal_kinds_in_tokens(&mac.mac) {
            if paths.iter().any(|p| p.name == kind) {
                continue;
            }
            let insert = paths
                .iter()
                .position(|p| p.name.contains("tool_panicked"))
                .unwrap_or(paths.len());
            paths.insert(insert, fail_item(&kind));
        }
    }
}

fn is_select_macro(m: &syn::ExprMacro) -> bool {
    m.mac
        .path
        .segments
        .last()
        .is_some_and(|s| s.ident == "select")
}

fn select_macro_in_stmt(stmt: &Stmt) -> Option<&syn::ExprMacro> {
    match stmt {
        Stmt::Local(local) => match local.init.as_ref().map(|i| &*i.expr) {
            Some(Expr::Macro(m)) if is_select_macro(m) => Some(m),
            _ => None,
        },
        Stmt::Expr(Expr::Macro(m), _) if is_select_macro(m) => Some(m),
        _ => None,
    }
}

fn fail_literal_kinds_in_tokens(mac: &syn::Macro) -> Vec<String> {
    let s = mac.tokens.to_string();
    let mut kinds = Vec::new();
    let mut rest = s.as_str();
    while let Some(idx) = rest.find("fail") {
        rest = &rest[idx + 4..];
        let Some(after) = rest.trim_start().strip_prefix('(') else {
            continue;
        };
        let after = after.trim_start();
        let Some(end) = after.find('"') else {
            continue;
        };
        let lit = &after[end + 1..];
        let Some(end_quote) = lit.find('"') else {
            continue;
        };
        kinds.push(lit[..end_quote].to_string());
        rest = &lit[end_quote + 1..];
    }
    kinds
}

fn fn_body(file: &syn::File, name: &str) -> Option<Vec<Stmt>> {
    fn walk(items: &[syn::Item], name: &str) -> Option<Vec<Stmt>> {
        for item in items {
            match item {
                syn::Item::Fn(f) if f.sig.ident == name => return Some(f.block.stmts.clone()),
                syn::Item::Impl(i) => {
                    for it in &i.items {
                        if let syn::ImplItem::Fn(f) = it {
                            if f.sig.ident == name {
                                return Some(f.block.stmts.clone());
                            }
                        }
                    }
                }
                syn::Item::Mod(m) => {
                    if let Some((_, inner)) = &m.content {
                        if let Some(found) = walk(inner, name) {
                            return Some(found);
                        }
                    }
                }
                _ => {}
            }
        }
        None
    }
    walk(&file.items, name)
}

struct FailPathVisitor<'a> {
    paths: &'a mut Vec<Item>,
}

impl<'a> FailPathVisitor<'a> {
    fn new(paths: &'a mut Vec<Item>) -> Self {
        Self { paths }
    }

    fn walk_stmts(&mut self, stmts: &[Stmt]) {
        for stmt in stmts {
            self.walk_stmt(stmt);
        }
    }

    fn walk_stmt(&mut self, stmt: &Stmt) {
        match stmt {
            Stmt::Local(local) => {
                if let Some(init) = &local.init {
                    if let Some((_, else_expr)) = &init.diverge {
                        if let Some(kind) = fail_kind_in_block_or_expr(else_expr) {
                            self.paths.push(fail_item(&kind));
                        }
                    }
                    self.walk_expr(&init.expr);
                }
            }
            Stmt::Expr(expr, _) => self.walk_expr(expr),
            _ => {}
        }
    }

    fn walk_expr(&mut self, expr: &Expr) {
        match expr {
            Expr::Let(let_expr) => {
                self.walk_expr(&let_expr.expr);
            }
            Expr::If(iff) => {
                if let Some(kind) = fail_kind_in_block(&iff.then_branch) {
                    self.paths.push(fail_item(&fail_label(&iff.cond, &kind)));
                } else {
                    self.walk_stmts(&iff.then_branch.stmts);
                }
                if let Some((_, else_branch)) = &iff.else_branch {
                    self.walk_expr(else_branch);
                }
            }
            Expr::Match(m) => {
                for arm in &m.arms {
                    if let Some(kind) = fail_kind_in_block_or_expr(&arm.body) {
                        self.paths
                            .push(fail_item(&match_arm_label(&arm.pat, &kind)));
                    } else if let Expr::Block(b) = &*arm.body {
                        self.walk_stmts(&b.block.stmts);
                    }
                }
            }
            Expr::Block(b) => self.walk_stmts(&b.block.stmts),
            Expr::Macro(m) if is_select_macro(m) => {
                let wrapped = format!("{{ {} }}", m.mac.tokens);
                if let Ok(block) = syn::parse_str::<Block>(&wrapped) {
                    self.walk_stmts(&block.stmts);
                }
            }
            _ => {}
        }
    }
}

fn fail_item(name: &str) -> Item {
    Item {
        name: name.into(),
        doc: String::new(),
        detail: "fail".into(),
    }
}

fn fail_label(cond: &Expr, kind: &str) -> String {
    if kind == "e.kind()" {
        match cond {
            Expr::Let(_) => format!("{}: {}", fail_cond(cond), kind),
            _ => kind.to_string(),
        }
    } else {
        kind.to_string()
    }
}

fn match_arm_label(pat: &syn::Pat, kind: &str) -> String {
    use syn::{Pat, PatLit, PatPath, PatTupleStruct};
    match pat {
        Pat::Path(PatPath { path, .. }) => {
            let tail = path
                .segments
                .last()
                .map(|s| s.ident.to_string())
                .unwrap_or_default();
            if kind == tail || kind == "e.kind()" {
                kind.to_string()
            } else {
                format!("{tail} → {kind}")
            }
        }
        Pat::TupleStruct(PatTupleStruct { path, .. }) => {
            let tail = path
                .segments
                .last()
                .map(|s| s.ident.to_string())
                .unwrap_or_default();
            format!("{tail} → {kind}")
        }
        Pat::Lit(PatLit { lit, .. }) => {
            if let syn::Lit::Str(s) = lit {
                format!("{} → {kind}", s.value())
            } else {
                kind.to_string()
            }
        }
        _ => kind.to_string(),
    }
}

fn fail_cond(e: &Expr) -> String {
    match e {
        Expr::Let(l) => fail_cond(&l.expr),
        Expr::MethodCall(m) => m.method.to_string(),
        Expr::Path(p) => p
            .path
            .segments
            .last()
            .map(|s| s.ident.to_string())
            .unwrap_or_default(),
        _ => String::new(),
    }
}

fn fail_kind_in_block_or_expr(body: &Expr) -> Option<String> {
    match body {
        Expr::Block(b) => fail_kind_in_block(&b.block),
        _ => fail_kind_in_expr(body),
    }
}

fn fail_kind_in_block(block: &Block) -> Option<String> {
    for stmt in &block.stmts {
        if let Some(kind) = fail_kind_in_stmt(stmt) {
            return Some(kind);
        }
    }
    None
}

fn fail_kind_in_stmt(stmt: &Stmt) -> Option<String> {
    match stmt {
        Stmt::Expr(Expr::Return(r), _) => fail_kind_in_expr(r.expr.as_ref()?),
        Stmt::Expr(expr, _) => fail_kind_in_expr(expr),
        _ => None,
    }
}

fn fail_kind_in_expr(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Return(r) => fail_kind_in_expr(r.expr.as_ref()?),
        Expr::Call(c) => fail_call(&c.func, &c.args),
        Expr::Block(b) => fail_kind_in_block(&b.block),
        Expr::Await(a) => fail_kind_in_expr(&a.base),
        _ => None,
    }
}

fn fail_call(
    func: &Expr,
    args: &syn::punctuated::Punctuated<Expr, syn::token::Comma>,
) -> Option<String> {
    let Expr::Path(p) = func else {
        return None;
    };
    if p.path.segments.last()?.ident != "fail" {
        return None;
    }
    let first = args.first()?;
    match first {
        Expr::Lit(l) => match &l.lit {
            syn::Lit::Str(s) => Some(s.value()),
            _ => None,
        },
        Expr::MethodCall(m) if m.method == "kind" => Some("e.kind()".into()),
        _ => None,
    }
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

    /// **If this breaks:** the endings page shows a rung `Ending` does not
    /// have, or omits one the empty-tool-calls branch still produces.
    #[test]
    fn the_endings_diagram_draws_the_four_variants_the_empty_tool_calls_branch_produces() {
        let root = root();
        let file = rust::parse(&root.join("crates/emma/src/agent.rs")).expect("agent.rs parses");
        let variants = rust::enum_variants(&file, "Ending").expect("Ending exists");

        let d = loop_endings(&root).expect("the diagram builds");
        let drawn: Vec<String> = d.layers.iter().flatten().map(|n| n.id.clone()).collect();
        for name in ["Done", "Stalled", "Answered", "KicksExhausted"] {
            assert!(
                variants.iter().any(|v| v.name == name),
                "{name} is not an Ending variant: {variants:?}"
            );
            assert!(
                drawn.contains(&name.to_string()),
                "{name} missing: {drawn:?}"
            );
        }
        assert_eq!(
            drawn,
            ["Done", "Stalled", "Answered", "KicksExhausted"],
            "check order, not enum declaration order: {drawn:?}"
        );
        assert!(
            d.caption.contains(&drawn.len().to_string()),
            "the caption states the count: {}",
            d.caption
        );
    }

    #[test]
    fn a_missing_source_fails_the_endings_diagram_rather_than_emptying_it() {
        let Err(e) = loop_endings(Path::new("definitely-not-a-workspace")) else {
            panic!("a missing tree must not produce a diagram");
        };
        let msg = format!("{e:#}");
        assert!(msg.contains("agent.rs"), "{msg}");
    }

    /// **If this breaks:** the failure page shows an exit `run_tool_call` no
    /// longer has, or omits one that still returns through fail.
    #[test]
    fn the_failure_diagram_draws_every_return_fail_in_source_order() {
        let root = root();
        let file = rust::parse(&root.join("crates/emma/src/agent.rs")).expect("agent.rs parses");
        let expected = fail_paths(&file, "run_tool_call").expect("failure paths");

        let d = loop_failure(&root).expect("the diagram builds");
        let drawn: Vec<String> = d.layers.iter().flatten().map(|n| n.id.clone()).collect();
        assert_eq!(
            drawn,
            expected.iter().map(|i| i.name.clone()).collect::<Vec<_>>(),
            "drawn order must match source order"
        );
        assert!(
            d.caption.contains(&expected.len().to_string()),
            "the caption states the count: {}",
            d.caption
        );
    }

    #[test]
    fn a_missing_source_fails_the_failure_diagram_rather_than_emptying_it() {
        let Err(e) = loop_failure(Path::new("definitely-not-a-workspace")) else {
            panic!("a missing tree must not produce a diagram");
        };
        let msg = format!("{e:#}");
        assert!(msg.contains("agent.rs"), "{msg}");
    }

    /// **If this breaks:** the one-turn page shows a step the loop no longer
    /// takes, or omits one it still does.
    #[test]
    fn the_one_turn_diagram_draws_the_guards_request_fields_and_placers() {
        let root = root();
        let agent = rust::parse(&root.join("crates/emma/src/agent.rs")).expect("agent.rs");
        let llm = rust::parse(&root.join("crates/llm/src/lib.rs")).expect("lib.rs");
        let endings = rust::enum_variants(&agent, "Ending").expect("Ending");
        for name in ["Interrupted", "Deadline", "Iterations"] {
            assert!(endings.iter().any(|v| v.name == name), "{name}");
        }
        let request = rust::struct_fields(&llm, "Request").expect("Request");
        for name in ["instructions", "tools", "history", "query"] {
            assert!(request.iter().any(|f| f.name == name), "{name}");
        }
        for name in ["compact_if_needed", "call_model", "place"] {
            assert!(rust::has_fn(&agent, name), "{name}");
        }

        let d = loop_one_turn(&root).expect("the diagram builds");
        let drawn: Vec<String> = d.layers.iter().flatten().map(|n| n.id.clone()).collect();
        assert_eq!(drawn.len(), 10, "{drawn:?}");
        for name in [
            "Interrupted",
            "Deadline",
            "Iterations",
            "compact_if_needed",
            "instructions",
            "tools",
            "history",
            "query",
            "call_model",
            "place",
        ] {
            assert!(
                drawn.contains(&name.to_string()),
                "{name} missing: {drawn:?}"
            );
        }
        assert!(
            d.caption.contains(&drawn.len().to_string()),
            "the caption states the count: {}",
            d.caption
        );
    }

    #[test]
    fn a_missing_source_fails_the_one_turn_diagram_rather_than_emptying_it() {
        let Err(e) = loop_one_turn(Path::new("definitely-not-a-workspace")) else {
            panic!("a missing tree must not produce a diagram");
        };
        let msg = format!("{e:#}");
        assert!(msg.contains("agent.rs") || msg.contains("lib.rs"), "{msg}");
    }

    /// **If this breaks:** the compaction page shows a guard the functions no
    /// longer check, or omits one they still do.
    #[test]
    fn the_compaction_diagram_draws_the_guards_both_functions_check() {
        let root = root();
        let file = rust::parse(&root.join("crates/emma/src/agent.rs")).expect("agent.rs");
        let mut expected = rust::fn_guards(&file, "compact_if_needed").expect("trigger guards");
        expected.extend(rust::fn_guards(&file, "compact").expect("compact guards"));

        let d = session_compaction(&root).expect("the diagram builds");
        let drawn: Vec<String> = d.layers.iter().flatten().map(|n| n.id.clone()).collect();
        assert_eq!(
            drawn,
            expected.iter().map(|i| i.name.clone()).collect::<Vec<_>>(),
            "guard order"
        );
        assert!(
            d.caption.contains(&expected.len().to_string()),
            "the caption states the count: {}",
            d.caption
        );
    }

    #[test]
    fn a_missing_source_fails_the_compaction_diagram_rather_than_emptying_it() {
        let Err(e) = session_compaction(Path::new("definitely-not-a-workspace")) else {
            panic!("a missing tree must not produce a diagram");
        };
        let msg = format!("{e:#}");
        assert!(msg.contains("agent.rs"), "{msg}");
    }

    #[test]
    fn the_memo_diagram_draws_the_writer_the_set_and_the_reader() {
        let root = root();
        let file = rust::parse(&root.join("crates/emma/src/agent.rs")).expect("agent.rs parses");
        for name in ["run_goal", "run_tool_call", "memo_key"] {
            assert!(rust::has_fn(&file, name), "{name} in agent.rs");
        }
        let fields = rust::struct_fields(&file, "Resumed").expect("Resumed exists");
        assert!(
            fields.iter().any(|f| f.name == "failed_now"),
            "failed_now on Resumed: {fields:?}"
        );

        let d = loop_memo(&root).expect("the diagram builds");
        let drawn: Vec<String> = d.layers.iter().flatten().map(|n| n.id.clone()).collect();
        assert_eq!(
            drawn,
            ["run_goal", "failed_now", "run_tool_call"],
            "{drawn:?}"
        );
        assert!(
            d.caption.contains(&drawn.len().to_string()),
            "the caption states the count: {}",
            d.caption
        );
    }

    #[test]
    fn a_missing_source_fails_the_memo_diagram_rather_than_emptying_it() {
        let Err(e) = loop_memo(Path::new("definitely-not-a-workspace")) else {
            panic!("a missing tree must not produce a diagram");
        };
        let msg = format!("{e:#}");
        assert!(msg.contains("agent.rs"), "{msg}");
    }

    #[test]
    fn the_fold_diagram_draws_the_fields_the_struct_declares() {
        let root = root();
        let file =
            rust::parse(&root.join("crates/emma/src/session.rs")).expect("session.rs parses");
        let fields = rust::struct_fields(&file, "Fold").expect("Fold exists");

        let d = session_fold(&root).expect("the diagram builds");
        let drawn: Vec<String> = d.layers.iter().flatten().map(|n| n.id.clone()).collect();
        assert_eq!(drawn.len(), fields.len(), "{drawn:?}");
        for f in &fields {
            assert!(drawn.contains(&f.name), "{} is missing: {drawn:?}", f.name);
        }
        assert!(
            d.caption.contains(&fields.len().to_string()),
            "the caption states the count: {}",
            d.caption
        );
    }

    #[test]
    fn a_missing_source_fails_the_fold_diagram_rather_than_emptying_it() {
        let Err(e) = session_fold(Path::new("definitely-not-a-workspace")) else {
            panic!("a missing tree must not produce a diagram");
        };
        let msg = format!("{e:#}");
        assert!(msg.contains("session.rs"), "{msg}");
    }
}
