//! Argument extraction, in the same words `tools/fs` uses.
//!
//! Deliberately a copy of the subset this crate needs rather than a dependency
//! on `emma-tools-fs`: the alternative is `tools/web` depending on the entire
//! filesystem tool surface — `Bash` included — to borrow four helpers, which
//! makes a crate that cannot write to disk depend on the one that exists to.
//! The cost of the copy is that a change to the phrasing must be made twice;
//! the phrasing is what the model learns, so a divergence would be visible
//! immediately rather than silently.

use serde_json::Value;

use emma_tool_api::ToolError;

fn bad(msg: impl Into<String>) -> ToolError {
    ToolError::BadArguments(msg.into())
}

pub fn object<'a>(
    args: &'a Value,
    tool: &str,
) -> Result<&'a serde_json::Map<String, Value>, ToolError> {
    args.as_object().ok_or_else(|| {
        bad(format!(
            "{tool} takes a JSON object, got {}",
            type_name(args)
        ))
    })
}

/// Unknown keys are refused rather than ignored. A silently dropped parameter
/// looks to the model exactly like a flag that had no effect, and it will
/// conclude the behaviour is impossible rather than that it misspelled the key.
pub fn deny_unknown(args: &Value, tool: &str, known: &[&str]) -> Result<(), ToolError> {
    let obj = object(args, tool)?;
    let mut unknown: Vec<&str> = obj
        .keys()
        .map(String::as_str)
        .filter(|k| !known.contains(k))
        .collect();
    if unknown.is_empty() {
        return Ok(());
    }
    unknown.sort_unstable();
    Err(bad(format!(
        "{tool} does not take {}; accepted parameters are {}",
        unknown.join(", "),
        known.join(", ")
    )))
}

pub fn req_str<'a>(args: &'a Value, tool: &str, key: &str) -> Result<&'a str, ToolError> {
    let obj = object(args, tool)?;
    match obj.get(key) {
        None => Err(bad(format!("{tool} requires {key}"))),
        Some(Value::String(s)) => Ok(s),
        Some(other) => Err(bad(format!(
            "{tool}.{key} must be a string, got {}",
            type_name(other)
        ))),
    }
}

pub fn opt_u64(args: &Value, tool: &str, key: &str) -> Result<Option<u64>, ToolError> {
    let obj = object(args, tool)?;
    match obj.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(n)) => match n.as_u64() {
            Some(v) => Ok(Some(v)),
            None => Err(bad(format!(
                "{tool}.{key} must be a non-negative whole number, got {n}"
            ))),
        },
        Some(other) => Err(bad(format!(
            "{tool}.{key} must be a number, got {}",
            type_name(other)
        ))),
    }
}

pub fn opt_str<'a>(args: &'a Value, tool: &str, key: &str) -> Result<Option<&'a str>, ToolError> {
    let obj = object(args, tool)?;
    match obj.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s)),
        Some(other) => Err(bad(format!(
            "{tool}.{key} must be a string, got {}",
            type_name(other)
        ))),
    }
}

/// A missing boolean is `None` and never `false`.
///
/// The distinction matters for a default that is `true` — `delta` on
/// `BrowserRead` — where collapsing absent into false would silently make every
/// read a full page re-send and nothing would look wrong.
pub fn opt_bool(args: &Value, tool: &str, key: &str) -> Result<Option<bool>, ToolError> {
    let obj = object(args, tool)?;
    match obj.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(b)) => Ok(Some(*b)),
        Some(other) => Err(bad(format!(
            "{tool}.{key} must be true or false, got {}",
            type_name(other)
        ))),
    }
}

pub fn type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}
