//! Diagrams for the Providers & models chapter of `docs/`.
//!
//! Pages: providers-boundary, providers-caching, providers-content,
//! providers-credentials, providers-floor, providers-models, providers-turn.
//!
//! One function per diagram, each naming the source file it reads and failing
//! if that file or the item in it is gone. `super::tools_lsp` is the worked
//! example; the rules for adding one are in this module's parent.

use std::path::Path;

use anyhow::{bail, Context, Result};
use syn::{Expr, Item};

use crate::rust::{self, Item as Fact};
use crate::shapes;
use crate::svg::Diagram;

fn fact(name: &str) -> Fact {
    Fact {
        name: name.into(),
        doc: String::new(),
        detail: String::new(),
    }
}

/// Wire keys `render` puts on the outgoing body, plus which ones are conditional.
///
/// The base keys come from the `json!` object in `AnthropicProvider::render`;
/// `output_config` and `stream` are inserted only on some requests. Sorted
/// alphabetically because that is how serde_json's map emits them.
fn render_body_keys(path: &Path) -> Result<(Vec<Fact>, Vec<String>)> {
    let file = rust::parse(path)?;
    if !rust::has_fn(&file, "render") {
        bail!("render is gone from {}", path.display());
    }
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("{} is a diagram's source", path.display()))?;
    let body = fn_body(&text, "fn render(")?;
    let mut keys = json_object_keys(&body)?;
    let conditional = ["output_config", "stream"]
        .into_iter()
        .filter(|k| body.contains(&format!("body[\"{k}\"]")))
        .map(str::to_string)
        .collect::<Vec<_>>();
    for k in &conditional {
        if !keys.iter().any(|f| f.name == *k) {
            keys.push(fact(k));
        }
    }
    keys.sort_by(|a, b| a.name.cmp(&b.name));
    Ok((keys, conditional))
}

fn fn_body(src: &str, sig: &str) -> Result<String> {
    let start = src.find(sig).with_context(|| format!("{sig} not found"))?;
    let rest = &src[start..];
    let open = rest.find('{').context("render has no opening brace")?;
    let mut depth = 0usize;
    for (i, ch) in rest[open..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Ok(rest[..open + i + 1].to_string());
                }
            }
            _ => {}
        }
    }
    bail!("unclosed brace in {sig}")
}

fn json_object_keys(body: &str) -> Result<Vec<Fact>> {
    let marker = "json!({";
    let start = body
        .find(marker)
        .context("render's json! object not found")?;
    let rest = &body[start + marker.len() - 1..];
    let mut depth = 0i32;
    let mut end = 0usize;
    for (i, ch) in rest.char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    end = i + 1;
                    break;
                }
            }
            _ => {}
        }
    }
    if end == 0 {
        bail!("json! object in render did not close");
    }
    let block = &rest[..end];
    let mut keys = Vec::new();
    for line in block.lines() {
        let line = line.trim().trim_end_matches(',');
        if let Some(rest) = line.strip_prefix('"') {
            if let Some((key, _)) = rest.split_once('"') {
                if !key.is_empty() && !keys.iter().any(|k: &Fact| k.name == key) {
                    keys.push(fact(key));
                }
            }
        }
    }
    if keys.is_empty() {
        bail!("no keys in render's json! object");
    }
    Ok(keys)
}

/// Effort levels one model row names, read from `models::TABLE`.
fn table_efforts(root: &Path, models_path: &Path, model: &str) -> Result<Vec<String>> {
    let lib = rust::parse(&root.join("crates/llm/src/lib.rs"))?;
    let models = rust::parse(models_path)?;
    let efforts = rust::enum_variants(&lib, "Effort")
        .with_context(|| "crates/llm/src/lib.rs declares Effort for rank comparisons")?;

    let row = table_row(&models, model).with_context(|| format!("no TABLE row for {model}"))?;
    let names = match row.as_str() {
        "ALL_FIVE" => efforts.iter().map(|e| e.name.clone()).collect(),
        "NO_XHIGH" => efforts
            .iter()
            .filter(|e| e.name != "XHigh")
            .map(|e| e.name.clone())
            .collect(),
        "empty" => Vec::new(),
        other => bail!("unknown efforts slice {other} on {model}"),
    };
    Ok(names)
}

fn table_row(file: &syn::File, model: &str) -> Result<String> {
    for item in &file.items {
        let Item::Const(c) = item else { continue };
        if c.ident != "TABLE" {
            continue;
        }
        let rows = match &*c.expr {
            Expr::Array(a) => &a.elems,
            Expr::Reference(r) => {
                let Expr::Array(a) = &*r.expr else {
                    bail!("TABLE is not an array");
                };
                &a.elems
            }
            _ => bail!("TABLE is not an array"),
        };
        for row in rows {
            let Expr::Tuple(pair) = row else { continue };
            let Some((Expr::Lit(lit), limits)) = pair.elems.first().zip(pair.elems.get(1)) else {
                continue;
            };
            let syn::Lit::Str(id) = &lit.lit else {
                continue;
            };
            if id.value() != model {
                continue;
            }
            return efforts_field(limits);
        }
    }
    bail!("TABLE not found")
}

fn efforts_field(limits: &Expr) -> Result<String> {
    let Expr::Struct(s) = limits else {
        bail!("TABLE row is not a Limits struct literal");
    };
    for fv in &s.fields {
        let syn::Member::Named(ident) = &fv.member else {
            continue;
        };
        if ident != "efforts" {
            continue;
        }
        return Ok(match &fv.expr {
            Expr::Path(p) => p
                .path
                .segments
                .last()
                .map(|seg| seg.ident.to_string())
                .unwrap_or_default(),
            Expr::Reference(r) if matches!(&*r.expr, Expr::Array(a) if a.elems.is_empty()) => {
                "empty".into()
            }
            _ => bail!("efforts field is neither ALL_FIVE, NO_XHIGH, nor &[]"),
        });
    }
    bail!("Limits has no efforts field")
}

/// String-literal arms of a `match` on one binding, in source order.
fn match_lit_arms(body: &str, match_on: &str) -> Result<Vec<String>> {
    let needle = format!("match {match_on} {{");
    let start = body
        .find(&needle)
        .with_context(|| format!("no `match {match_on}` in this body"))?;
    let rest = &body[start + needle.len()..];
    let mut depth = 1i32;
    let mut end = 0usize;
    for (i, ch) in rest.char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    end = i;
                    break;
                }
            }
            _ => {}
        }
    }
    if end == 0 {
        bail!("`match {match_on}` did not close");
    }
    let block = &rest[..end];
    let mut arms = Vec::new();
    for line in block.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix('"') else {
            continue;
        };
        let Some((lit, after)) = rest.split_once('"') else {
            continue;
        };
        if after.trim_start().starts_with("=>") && !lit.is_empty() {
            arms.push(lit.to_string());
        }
    }
    if arms.is_empty() {
        bail!("`match {match_on}` has no string-literal arms");
    }
    Ok(arms)
}

fn count_in_fn(src: &str, sig: &str, needle: &str) -> Result<usize> {
    Ok(fn_body(src, sig)?.matches(needle).count())
}

fn min_cacheable_rows(file: &syn::File) -> Result<Vec<(String, usize)>> {
    for item in &file.items {
        let Item::Const(c) = item else { continue };
        if c.ident != "MIN_CACHEABLE" {
            continue;
        }
        let rows = match &*c.expr {
            Expr::Array(a) => &a.elems,
            Expr::Reference(r) => {
                let Expr::Array(a) = &*r.expr else {
                    bail!("MIN_CACHEABLE is not an array");
                };
                &a.elems
            }
            _ => bail!("MIN_CACHEABLE is not an array"),
        };
        let mut out = Vec::new();
        for row in rows {
            let Expr::Tuple(pair) = row else { continue };
            let Some((Expr::Lit(id_lit), floor_expr)) = pair.elems.first().zip(pair.elems.get(1))
            else {
                continue;
            };
            let syn::Lit::Str(id) = &id_lit.lit else {
                continue;
            };
            let floor = match floor_expr {
                Expr::Lit(l) => {
                    let syn::Lit::Int(i) = &l.lit else {
                        continue;
                    };
                    i.base10_parse()?
                }
                _ => continue,
            };
            out.push((id.value(), floor));
        }
        if out.is_empty() {
            bail!("MIN_CACHEABLE has no rows");
        }
        return Ok(out);
    }
    bail!("MIN_CACHEABLE not found")
}

fn lookup_floor(rows: &[(String, usize)], model: &str, unknown: usize) -> usize {
    rows.iter()
        .filter(|(id, _)| model.starts_with(id))
        .max_by_key(|(id, _)| id.len())
        .map(|(_, floor)| *floor)
        .unwrap_or(unknown)
}

/// Estimated tokens in `big_instructions()`, from that test helper's repeat count.
fn big_instructions_tokens(src: &str, chars_per_token: usize) -> Result<usize> {
    let start = src
        .find("fn big_instructions()")
        .context("big_instructions not found")?;
    let chunk = &src[start..start.saturating_add(220)];
    let rest = chunk
        .split(".repeat(")
        .nth(1)
        .context("big_instructions repeat count")?;
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    let repeat: usize = digits.parse().context("big_instructions repeat count")?;
    Ok("You are Emma. ".len() * repeat / chars_per_token)
}

fn clamp_effort(supported: &[String], requested: &str) -> Option<String> {
    let rank = |name: &str| -> u8 {
        match name {
            "Low" => 0,
            "Medium" => 1,
            "High" => 2,
            "XHigh" => 3,
            "Max" => 4,
            _ => 255,
        }
    };
    let req = rank(requested);
    supported
        .iter()
        .filter(|e| rank(e) <= req)
        .max_by_key(|e| rank(e))
        .cloned()
}

/// What `render` emits on the wire for a [`Request`], read from `anthropic.rs`.
///
/// One source fans out to sorted body keys; conditional keys are dashed because
/// `output_config` and `stream` are absent on some requests.
pub fn providers_boundary(root: &Path) -> Result<Diagram> {
    let lib = root.join("crates/llm/src/lib.rs");
    let anthropic = root.join("crates/llm/src/anthropic.rs");
    let lib_file = rust::parse(&lib)?;
    let fields = rust::struct_fields(&lib_file, "Request")
        .with_context(|| format!("{} declares Request", lib.display()))?;
    let (keys, conditional) = render_body_keys(&anthropic)?;

    let n_fields = fields.len();
    let n_keys = keys.len();
    let dim: Vec<&str> = conditional.iter().map(String::as_str).collect();
    Ok(shapes::fan(
        "bnd",
        format!(
            "A Request's {n_fields} fields are rendered into a body whose \
             {n_keys} keys are sorted for the cache. Pink marks keys that may \
             be absent: {}.",
            if conditional.is_empty() {
                "none".into()
            } else {
                conditional.join(", ")
            }
        ),
        &fact("Request"),
        &keys,
        &dim,
    ))
}

/// Credential resolution steps in `auth.rs`, in the order the file path needs them.
///
/// The environment branch in `load_default` returns before this ladder runs; the
/// caption on the page names that split.
pub fn providers_credentials(root: &Path) -> Result<Diagram> {
    let path = root.join("crates/llm/src/auth.rs");
    let file = rust::parse(&path)?;
    let steps = ["from_env", "home_dir", "credentials_path", "load_file"];
    let mut items = Vec::new();
    for name in steps {
        if !rust::has_fn(&file, name) {
            bail!("{name} is gone from {}", path.display());
        }
        items.push(fact(name));
    }
    let n = items.len();
    Ok(shapes::ladder(
        "crd",
        format!(
            "The {n} functions on the stored-file path after the environment \
             check fails: {}. The current working directory is not consulted \
             at any step.",
            steps.join(" → ")
        ),
        &items,
        None,
    ))
}

/// How `clamp_effort` answers an `XHigh` ask on three rows from `TABLE`.
pub fn providers_models(root: &Path) -> Result<Diagram> {
    let path = root.join("crates/llm/src/models.rs");
    let file = rust::parse(&path)?;
    if !rust::has_fn(&file, "clamp_effort") {
        bail!("clamp_effort is gone from {}", path.display());
    }
    let ask = "XHigh";
    let models = ["claude-opus-5", "claude-opus-4-6", "claude-haiku-4-5"];
    let mut outcomes = Vec::new();
    let mut dim = Vec::new();
    for model in models {
        let supported = table_efforts(root, &path, model)?;
        let clamped = clamp_effort(&supported, ask);
        let (label, is_dim) = match clamped {
            Some(e) => (format!("effort: \"{}\"", e.to_lowercase()), false),
            None => ("no output_config".into(), true),
        };
        outcomes.push(Fact {
            name: model.to_string(),
            doc: String::new(),
            detail: label,
        });
        if is_dim {
            dim.push(model);
        }
    }
    Ok(shapes::fan(
        "mdl",
        format!(
            "An {ask} ask from Request::new against {} models: {}.",
            models.len(),
            outcomes
                .iter()
                .map(|o| format!("{} → {}", o.name, o.detail))
                .collect::<Vec<_>>()
                .join("; "),
        ),
        &fact(ask),
        &outcomes,
        &dim,
    ))
}

/// Where the three cache breakpoints sit in the rendered prefix, read from
/// `render`'s field order, `cache_control` in `system_field`, and
/// `mark_breakpoint` calls in `messages_field`.
pub fn providers_caching(root: &Path) -> Result<Diagram> {
    let path = root.join("crates/llm/src/anthropic.rs");
    let file = rust::parse(&path)?;
    for name in [
        "render",
        "system_field",
        "messages_field",
        "mark_breakpoint",
    ] {
        if !rust::has_fn(&file, name) {
            bail!("{name} is gone from {}", path.display());
        }
    }
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("{} is a diagram's source", path.display()))?;
    let bp_system = count_in_fn(&text, "fn system_field(", "cache_control")?;
    let bp_messages = count_in_fn(&text, "fn messages_field(", "mark_breakpoint")?;
    let breakpoints = bp_system + bp_messages;

    let steps = vec![
        Fact {
            name: "tools[]".into(),
            doc: String::new(),
            detail: "counted, never marked".into(),
        },
        Fact {
            name: "system[0]".into(),
            doc: String::new(),
            detail: format!("{bp_system} breakpoint"),
        },
        Fact {
            name: "messages".into(),
            doc: String::new(),
            detail: format!("{bp_messages} breakpoints"),
        },
    ];
    let n = steps.len();
    Ok(shapes::ladder(
        "cch",
        format!(
            "The server renders {n} prefix sections in order — tools, then system, \
             then messages — with {breakpoints} cache_control breakpoints total: \
             {bp_system} on the system block and {bp_messages} in the message list, \
             each gated on clears_minimum."
        ),
        &steps,
        Some("system[0]"),
    ))
}

/// Every path through `ContentBlock::from_value`, read as the checks it makes
/// and the string-literal arms of its `match kind`.
pub fn providers_content(root: &Path) -> Result<Diagram> {
    let path = root.join("crates/llm/src/content.rs");
    let file = rust::parse(&path)?;
    if !rust::has_fn(&file, "from_value") {
        bail!("from_value is gone from {}", path.display());
    }
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("{} is a diagram's source", path.display()))?;
    let body = fn_body(&text, "pub fn from_value(")?;
    let typed_arms = match_lit_arms(&body, "kind")?;
    let passthrough_returns = body.matches("Self::Passthrough").count();

    let steps = vec![
        Fact {
            name: r#""type" tag"#.into(),
            doc: String::new(),
            detail: "else Passthrough".into(),
        },
        Fact {
            name: "JSON object".into(),
            doc: String::new(),
            detail: "else Passthrough".into(),
        },
        Fact {
            name: "deserialize".into(),
            doc: String::new(),
            detail: format!("{} typed arms", typed_arms.len()),
        },
        Fact {
            name: "Passthrough".into(),
            doc: String::new(),
            detail: format!("{passthrough_returns} paths"),
        },
        Fact {
            name: "typed variant".into(),
            doc: String::new(),
            detail: typed_arms.join(", "),
        },
    ];
    let n_checks = 3usize;
    Ok(shapes::ladder(
        "cnt",
        format!(
            "ContentBlock::from_value makes {n_checks} checks then branches: \
             {passthrough_returns} paths reach Passthrough; the rest reach one of \
             {n_typed} typed arms ({arms}). Unmodelled keys on a known type stay \
             typed via serde(flatten).",
            n_typed = typed_arms.len(),
            arms = typed_arms.join(", "),
        ),
        &steps,
        Some("typed variant"),
    ))
}

/// How the same estimated prefix clears different model floors, from
/// `MIN_CACHEABLE`, `MIN_CACHEABLE_UNKNOWN`, and the `big_instructions` fixture.
pub fn providers_floor(root: &Path) -> Result<Diagram> {
    let path = root.join("crates/llm/src/anthropic.rs");
    let file = rust::parse(&path)?;
    if !rust::has_fn(&file, "min_cacheable_tokens") {
        bail!("min_cacheable_tokens is gone from {}", path.display());
    }
    let rows = min_cacheable_rows(&file)?;
    let unknown: usize = rust::const_value(&file, "MIN_CACHEABLE_UNKNOWN")?.parse()?;
    let chars_per_token: usize = rust::const_value(&file, "CHARS_PER_TOKEN")?.parse()?;
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("{} is a diagram's source", path.display()))?;
    let prefix = big_instructions_tokens(&text, chars_per_token)?;

    let models = [
        "claude-opus-5",
        "claude-sonnet-5",
        "claude-haiku-4-5",
        "unknown-model",
    ];
    let mut outcomes = Vec::new();
    let mut dim = Vec::new();
    for model in models {
        let floor = lookup_floor(&rows, model, unknown);
        let marked = prefix >= floor;
        if !marked {
            dim.push(model);
        }
        outcomes.push(Fact {
            name: model.to_string(),
            doc: String::new(),
            detail: if marked {
                format!("marked (floor {floor})")
            } else {
                format!("unmarked (floor {floor})")
            },
        });
    }

    Ok(shapes::fan(
        "flr",
        format!(
            "One prefix of about {prefix} estimated tokens (chars/{chars_per_token}) \
             on {n} models: {}. Unknown models use MIN_CACHEABLE_UNKNOWN ({unknown}).",
            outcomes
                .iter()
                .map(|o| format!("{} → {}", o.name, o.detail))
                .collect::<Vec<_>>()
                .join("; "),
            n = models.len(),
        ),
        &Fact {
            name: format!("~{prefix} tokens"),
            doc: String::new(),
            detail: String::new(),
        },
        &outcomes,
        &dim,
    ))
}

/// How `Assembly::apply` routes SSE frames, read from its `match` on frame type.
pub fn providers_turn(root: &Path) -> Result<Diagram> {
    let path = root.join("crates/llm/src/anthropic.rs");
    let file = rust::parse(&path)?;
    if !rust::has_fn(&file, "apply") {
        bail!("Assembly::apply is gone from {}", path.display());
    }
    if !rust::has_fn(&file, "finish") {
        bail!("Assembly::finish is gone from {}", path.display());
    }
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("{} is a diagram's source", path.display()))?;
    let body = fn_body(&text, "async fn apply(")?;
    let frame_arms = [
        "message_start",
        "content_block_start",
        "content_block_delta",
        "message_delta",
        "error",
    ];
    let mut arms = Vec::new();
    for lit in frame_arms {
        if body.contains(&format!("\"{lit}\" =>")) {
            arms.push(lit.to_string());
        }
    }
    if arms.len() != frame_arms.len() {
        bail!(
            "Assembly::apply is missing an SSE frame arm: found {arms:?}, expected {frame_arms:?}"
        );
    }

    let destinations: &[(&str, &str)] = &[
        ("message_start", "Assembly.usage"),
        ("content_block_start", "blocks[index]"),
        ("content_block_delta", "Partial append"),
        ("message_delta", "stop_reason + output"),
        ("error", "Err(LlmError::Api)"),
    ];
    let mut steps = Vec::new();
    for arm in &arms {
        let detail = destinations
            .iter()
            .find(|(name, _)| *name == arm.as_str())
            .map(|(_, dest)| (*dest).to_string())
            .unwrap_or_else(|| "dropped".into());
        steps.push(Fact {
            name: arm.clone(),
            doc: String::new(),
            detail,
        });
    }
    steps.push(Fact {
        name: "finish()".into(),
        doc: String::new(),
        detail: "turn_from_content".into(),
    });

    let n = arms.len();
    Ok(shapes::ladder(
        "trn",
        format!(
            "SSE frames route through Assembly::apply's {n} typed arms ({arms}), \
             then finish() hands the assembled blocks to turn_from_content — the \
             same function the batch path uses.",
            arms = arms.join(", "),
        ),
        &steps,
        Some("finish()"),
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

    // --- providers-boundary ---

    #[test]
    fn the_boundary_diagram_draws_request_fields_and_render_keys_from_source() {
        let root = root();
        let lib = root.join("crates/llm/src/lib.rs");
        let anthropic = root.join("crates/llm/src/anthropic.rs");
        let fields = rust::struct_fields(&rust::parse(&lib).expect("lib.rs"), "Request")
            .expect("Request exists");
        let (keys, _) = render_body_keys(&anthropic).expect("render keys parse");

        let d = providers_boundary(&root).expect("the diagram builds");
        assert_eq!(d.layers[0][0].id, "Request");
        let drawn: Vec<String> = d.layers[1].iter().map(|n| n.id.clone()).collect();
        assert_eq!(drawn.len(), keys.len(), "{drawn:?}");
        for k in &keys {
            assert!(drawn.contains(&k.name), "{} missing: {drawn:?}", k.name);
        }
        assert!(
            d.caption.contains(&fields.len().to_string()),
            "caption states the field count: {}",
            d.caption
        );
        assert!(drawn.contains(&"max_tokens".to_string()));
        assert!(drawn.contains(&"messages".to_string()));
    }

    #[test]
    fn a_missing_boundary_source_fails_rather_than_emptying_the_diagram() {
        let Err(e) = providers_boundary(Path::new("definitely-not-a-workspace")) else {
            panic!("a missing tree must not produce a diagram");
        };
        let msg = format!("{e:#}");
        assert!(
            msg.contains("lib.rs") || msg.contains("anthropic.rs"),
            "{msg}"
        );
    }

    // --- providers-credentials ---

    #[test]
    fn the_credentials_diagram_draws_the_resolution_functions_auth_declares() {
        let root = root();
        let path = root.join("crates/llm/src/auth.rs");
        let file = rust::parse(&path).expect("auth.rs parses");
        let expected = ["from_env", "home_dir", "credentials_path", "load_file"];
        for name in expected {
            assert!(rust::has_fn(&file, name), "{name} must exist");
        }

        let d = providers_credentials(&root).expect("the diagram builds");
        let drawn: Vec<String> = d.layers.iter().flatten().map(|n| n.id.clone()).collect();
        assert_eq!(drawn.len(), expected.len(), "{drawn:?}");
        for name in expected {
            assert!(
                drawn.contains(&name.to_string()),
                "{name} missing: {drawn:?}"
            );
        }
        assert!(
            d.caption.contains(&expected.len().to_string()),
            "caption states the step count: {}",
            d.caption
        );
    }

    #[test]
    fn a_missing_credentials_source_fails_rather_than_emptying_the_diagram() {
        let Err(e) = providers_credentials(Path::new("definitely-not-a-workspace")) else {
            panic!("a missing tree must not produce a diagram");
        };
        let msg = format!("{e:#}");
        assert!(msg.contains("auth.rs"), "{msg}");
    }

    // --- providers-models ---

    #[test]
    fn the_models_diagram_draws_clamp_effort_outcomes_from_table_rows() {
        let root = root();
        let path = root.join("crates/llm/src/models.rs");
        let models = ["claude-opus-5", "claude-opus-4-6", "claude-haiku-4-5"];
        let mut expected = Vec::new();
        for model in models {
            let supported = table_efforts(&root, &path, model).expect("TABLE row");
            let clamped = clamp_effort(&supported, "XHigh").map(|e| e.to_lowercase());
            expected.push((model, clamped));
        }

        let d = providers_models(&root).expect("the diagram builds");
        assert_eq!(d.layers[0][0].id, "XHigh");
        let drawn: Vec<(String, String)> = d.layers[1]
            .iter()
            .map(|n| (n.id.clone(), n.label.clone()))
            .collect();
        assert_eq!(drawn.len(), models.len(), "{drawn:?}");
        for (model, clamped) in expected {
            assert!(
                drawn.iter().any(|(id, _)| id == model),
                "{model} missing: {drawn:?}"
            );
            if let Some(e) = clamped {
                assert!(
                    d.caption.contains(model) && d.caption.contains(&e),
                    "caption should name {model} sending {e}: {}",
                    d.caption
                );
            } else {
                assert!(
                    d.caption.contains(model) && d.caption.contains("no output_config"),
                    "caption should name {model} omitting output_config: {}",
                    d.caption
                );
            }
        }
    }

    #[test]
    fn a_missing_models_source_fails_rather_than_emptying_the_diagram() {
        let Err(e) = providers_models(Path::new("definitely-not-a-workspace")) else {
            panic!("a missing tree must not produce a diagram");
        };
        let msg = format!("{e:#}");
        assert!(msg.contains("models.rs"), "{msg}");
    }

    // --- providers-caching ---

    #[test]
    fn the_caching_diagram_draws_breakpoints_from_mark_breakpoint_calls() {
        let root = root();
        let path = root.join("crates/llm/src/anthropic.rs");
        let text = std::fs::read_to_string(&path).expect("anthropic.rs");
        let bp_system = count_in_fn(&text, "fn system_field(", "cache_control").expect("system");
        let bp_messages =
            count_in_fn(&text, "fn messages_field(", "mark_breakpoint").expect("messages");

        let d = providers_caching(&root).expect("the diagram builds");
        let drawn: Vec<String> = d.layers.iter().flatten().map(|n| n.id.clone()).collect();
        assert_eq!(drawn.len(), 3, "{drawn:?}");
        assert!(
            d.caption.contains(&(bp_system + bp_messages).to_string()),
            "caption states the breakpoint count: {}",
            d.caption
        );
    }

    #[test]
    fn a_missing_caching_source_fails_rather_than_emptying_the_diagram() {
        let Err(e) = providers_caching(Path::new("definitely-not-a-workspace")) else {
            panic!("a missing tree must not produce a diagram");
        };
        assert!(format!("{e:#}").contains("anthropic.rs"));
    }

    // --- providers-content ---

    #[test]
    fn the_content_diagram_draws_from_value_checks_and_match_arms() {
        let root = root();
        let path = root.join("crates/llm/src/content.rs");
        let text = std::fs::read_to_string(&path).expect("content.rs");
        let body = fn_body(&text, "pub fn from_value(").expect("from_value");
        let arms = match_lit_arms(&body, "kind").expect("kind arms");

        let d = providers_content(&root).expect("the diagram builds");
        for arm in &arms {
            assert!(
                d.caption.contains(arm),
                "caption names {arm}: {}",
                d.caption
            );
        }
        assert!(
            d.caption.contains(&arms.len().to_string()),
            "caption states the typed arm count: {}",
            d.caption
        );
    }

    #[test]
    fn a_missing_content_source_fails_rather_than_emptying_the_diagram() {
        let Err(e) = providers_content(Path::new("definitely-not-a-workspace")) else {
            panic!("a missing tree must not produce a diagram");
        };
        assert!(format!("{e:#}").contains("content.rs"));
    }

    // --- providers-floor ---

    #[test]
    fn the_floor_diagram_draws_min_cacheable_outcomes_for_big_instructions() {
        let root = root();
        let path = root.join("crates/llm/src/anthropic.rs");
        let file = rust::parse(&path).expect("anthropic.rs");
        let rows = min_cacheable_rows(&file).expect("MIN_CACHEABLE");
        let unknown: usize = rust::const_value(&file, "MIN_CACHEABLE_UNKNOWN")
            .expect("unknown")
            .parse()
            .expect("parse");
        let chars_per_token: usize = rust::const_value(&file, "CHARS_PER_TOKEN")
            .expect("chars")
            .parse()
            .expect("parse");
        let text = std::fs::read_to_string(&path).expect("read");
        let prefix = big_instructions_tokens(&text, chars_per_token).expect("prefix");

        let d = providers_floor(&root).expect("the diagram builds");
        assert_eq!(d.layers[0][0].id, format!("~{prefix} tokens"));
        let opus_floor = lookup_floor(&rows, "claude-opus-5", unknown);
        let haiku_floor = lookup_floor(&rows, "claude-haiku-4-5", unknown);
        assert!(prefix >= opus_floor);
        assert!(prefix < haiku_floor);
    }

    #[test]
    fn a_missing_floor_source_fails_rather_than_emptying_the_diagram() {
        let Err(e) = providers_floor(Path::new("definitely-not-a-workspace")) else {
            panic!("a missing tree must not produce a diagram");
        };
        assert!(format!("{e:#}").contains("anthropic.rs"));
    }

    // --- providers-turn ---

    #[test]
    fn the_turn_diagram_draws_assembly_apply_frame_arms_in_order() {
        let root = root();
        let path = root.join("crates/llm/src/anthropic.rs");
        let text = std::fs::read_to_string(&path).expect("anthropic.rs");
        let body = fn_body(&text, "async fn apply(").expect("apply");
        for lit in [
            "message_start",
            "content_block_start",
            "content_block_delta",
            "message_delta",
            "error",
        ] {
            assert!(body.contains(&format!("\"{lit}\" =>")), "{lit} arm");
        }

        let d = providers_turn(&root).expect("the diagram builds");
        let drawn: Vec<String> = d.layers.iter().flatten().map(|n| n.id.clone()).collect();
        assert!(
            drawn.first().is_some_and(|s| s == "message_start"),
            "{drawn:?}"
        );
        assert!(drawn.contains(&"finish()".to_string()), "{drawn:?}");
        assert!(d.caption.contains("turn_from_content"), "{}", d.caption);
    }

    #[test]
    fn a_missing_turn_source_fails_rather_than_emptying_the_diagram() {
        let Err(e) = providers_turn(Path::new("definitely-not-a-workspace")) else {
            panic!("a missing tree must not produce a diagram");
        };
        assert!(format!("{e:#}").contains("anthropic.rs"));
    }
}
