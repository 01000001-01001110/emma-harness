//! Diagrams for the Providers & models chapter of `docs/`.
//!
//! Pages: providers-boundary, providers-content, providers-credentials, providers-models, providers-turn.
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
}
