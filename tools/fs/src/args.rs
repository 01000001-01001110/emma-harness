//! Argument extraction, shared so that every tool rejects a malformed call in
//! the same words. The model learns the shape of one error message and can
//! then read all of them.
//!
//! Every message names the tool, the parameter, and what was actually supplied
//! — models correct a typed mistake reliably and guess at an untyped one. Two
//! conventions run through the whole file: an explicit JSON `null` is treated
//! as absent rather than as a wrong type, because that is what a model emits
//! when it means "not this time"; and everything here is pure, so it can run in
//! `validate_args` before any filesystem is touched.

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

/// Unknown keys are refused rather than ignored. A silently dropped
/// `replace_all` or `timeout_ms` looks to the model exactly like a flag that
/// had no effect, and it will conclude the behaviour is impossible rather than
/// that it misspelled the key.
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

/// Integers only, and non-negative. A float here means the model computed the
/// value rather than stating it, which is worth surfacing.
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
